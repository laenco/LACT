mod mock_fs;

use crate::{
    config::Config,
    server::handler::{Handler, read_pci_db},
};
use insta::{assert_debug_snapshot, assert_json_snapshot};
use lact_schema::config::{GpuConfig, PowerLimitThrottle};
use mock_fs::MockSysfs;
use std::{
    ffi::OsStr,
    fs,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use tempfile::tempdir;

fn init_tracing() {
    static TRACING_LOCK: OnceLock<()> = OnceLock::new();
    TRACING_LOCK.get_or_init(|| {
        tracing_subscriber::fmt()
            .with_env_filter("easy_fuser=warn,info")
            .init();
    });
}

#[tokio::test(flavor = "local")]
#[cfg_attr(miri, ignore)]
async fn snapshot_everything() {
    init_tracing();

    let test_data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/snapshots");
    let pci_db = read_pci_db();

    for vendor_dir in fs::read_dir(test_data_dir).unwrap().flatten() {
        if !vendor_dir.file_type().unwrap().is_dir() {
            continue;
        }

        for device_dir in fs::read_dir(vendor_dir.path()).unwrap().flatten() {
            let test_key = format!(
                "{}/{}",
                vendor_dir.file_name().to_string_lossy(),
                device_dir.file_name().to_string_lossy()
            );

            let handler = Handler::with_base_path(&device_dir.path(), Config::default(), &pci_db)
                .await
                .unwrap();
            let device_info = handler
                .generate_snapshot_device_info()
                .await
                .into_values()
                .next()
                .unwrap();
            let device_info = serde_json::to_value(&device_info).unwrap();

            assert_json_snapshot!(test_key, device_info);
        }
    }
}

#[tokio::test(flavor = "local")]
#[cfg_attr(miri, ignore)]
async fn apply_settings() {
    init_tracing();

    let test_data_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/snapshots");
    let pci_db = read_pci_db();

    for vendor_dir in fs::read_dir(test_data_dir).unwrap().flatten() {
        if !vendor_dir.metadata().unwrap().is_dir() {
            continue;
        }

        for device_dir in fs::read_dir(vendor_dir.path()).unwrap().flatten() {
            for entry in fs::read_dir(device_dir.path()).unwrap().flatten() {
                let name = entry.file_name();
                let name = name.to_str().unwrap();

                #[allow(clippy::case_sensitive_file_extension_comparisons)]
                if name.starts_with("config") && name.ends_with(".yaml") {
                    let test_key = format!(
                        "apply_config/{}/{}/{name}",
                        vendor_dir.file_name().to_string_lossy(),
                        device_dir.file_name().to_string_lossy()
                    );
                    let raw_gpu_config = fs::read_to_string(entry.path()).unwrap();
                    let gpu_config: GpuConfig = serde_norway::from_str(&raw_gpu_config).unwrap();

                    let mock_fs_dir = tempdir().unwrap();

                    let mock_fs = MockSysfs::new(device_dir.path());
                    let writes = mock_fs.writes.clone();

                    let mount = easy_fuser::fuse_parallel::spawn_mount(
                        mock_fs,
                        mock_fs_dir.path(),
                        &[],
                        Some(1),
                    )
                    .expect("Could not mount mock fs");

                    let handler =
                        Handler::with_base_path(mock_fs_dir.path(), Config::default(), &pci_db)
                            .await
                            .unwrap();
                    let gpu_id = &handler.list_devices().await[0].id;

                    handler
                        .config
                        .write()
                        .await
                        .gpus_mut()
                        .unwrap()
                        .insert(gpu_id.clone(), gpu_config);

                    handler.apply_current_config().await.unwrap();

                    mount.join();
                    mock_fs_dir.close().unwrap();

                    let write_commands = writes
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|(name, contents)| format!("{}: {contents}", name.to_str().unwrap()))
                        .collect::<Vec<String>>();
                    assert_debug_snapshot!(test_key, write_commands);
                }
            }
        }
    }
}

/// Checks that an enabled power limit throttling lowers the power limit of a device
/// which is over one of its temperature thresholds, and that the limit is not left
/// lowered when the feature is disabled again.
#[tokio::test(flavor = "local")]
#[cfg_attr(miri, ignore)]
async fn power_limit_throttling() {
    init_tracing();

    let device_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/snapshots")
        .join("amd/rx7900xtx");
    let pci_db = read_pci_db();

    let mock_fs_dir = tempdir().unwrap();

    let mock_fs = MockSysfs::new(device_dir);
    let writes = mock_fs.writes.clone();

    let mount = easy_fuser::fuse_parallel::spawn_mount(mock_fs, mock_fs_dir.path(), &[], Some(1))
        .expect("Could not mount mock fs");

    let handler = Handler::with_base_path(mock_fs_dir.path(), Config::default(), &pci_db)
        .await
        .unwrap();
    let gpu_id = handler.list_devices().await[0].id.clone();

    // The snapshot of this device has a junction temperature of 61 degrees
    // and a default power limit of 290W.
    let gpu_config = GpuConfig {
        power_limit_throttle: Some(PowerLimitThrottle {
            min_power: 261.0,
            thresholds: [(("junction".to_owned()), 55)].into(),
            step: 10.0,
            interval_secs: 1,
            hysteresis: 5,
            load_lock: None,
        }),
        ..GpuConfig::default()
    };

    handler
        .config
        .write()
        .await
        .gpus_mut()
        .unwrap()
        .insert(gpu_id.clone(), gpu_config);

    handler.apply_current_config().await.unwrap();

    // Give the throttling task a few intervals to lower the power limit.
    tokio::time::sleep(Duration::from_millis(3500)).await;

    let cap_writes = power_cap_writes(&writes);
    assert_eq!(
        cap_writes.first().expect("No power limit was written"),
        "280000000",
        "power limit was not lowered by one step, wrote: {cap_writes:?}"
    );
    assert!(
        cap_writes
            .iter()
            .all(|cap| cap.parse::<f64>().unwrap_or(0.0) >= 261_000_000.0),
        "power limit was lowered below the configured minimum: {cap_writes:?}"
    );

    let stats = handler.get_gpu_stats(&gpu_id).await.unwrap();
    let throttle_status = stats
        .power_limit_throttle
        .expect("No power limit throttling status");
    assert_eq!(throttle_status.base_cap, 290.0);
    assert!(throttle_status.effective_cap < throttle_status.base_cap);
    assert!(throttle_status.effective_cap >= 261.0);
    assert_eq!(throttle_status.breached, vec!["junction".to_owned()]);

    handler.cleanup().await;

    let cap_writes = power_cap_writes(&writes);
    assert_eq!(
        cap_writes.last().expect("No power limit was written"),
        "290000000",
        "power limit was not restored when the throttling was stopped"
    );

    let stats = handler.get_gpu_stats(&gpu_id).await.unwrap();
    assert!(stats.power_limit_throttle.is_none());

    mount.join();
    mock_fs_dir.close().unwrap();
}

fn power_cap_writes(writes: &Arc<Mutex<Vec<(PathBuf, String)>>>) -> Vec<String> {
    writes
        .lock()
        .unwrap()
        .iter()
        .filter(|(path, _)| path.file_name() == Some(OsStr::new("power1_cap")))
        .map(|(_, contents)| contents.trim().to_owned())
        .collect()
}
