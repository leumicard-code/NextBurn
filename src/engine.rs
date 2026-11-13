// ============================================================================
// engine.rs — NextBurn Core Engine (USB Discovery, ISO Inspection & Burners)
// ============================================================================

#![allow(dead_code)]

use anyhow::{bail, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;
use std::time::{Duration, Instant};

pub const IO_CHUNK_SIZE: usize = 4 * 1024 * 1024; // 4MB Buffer
pub const SYNC_INTERVAL: u64 = 32 * 1024 * 1024;  // Flush hardware cache every 32MB

// ============================================================================
// 1. Data Models
// ============================================================================

#[derive(Clone, Debug, PartialEq)]
pub struct UsbDrive {
    pub device: PathBuf,
    pub name: String,
    pub size_gb: f64,
    pub vendor: String,
    pub model: String,
}

impl UsbDrive {
    pub fn display_name(&self) -> String {
        let v = self.vendor.trim();
        let m = self.model.trim();
        let desc = match (v.is_empty(), m.is_empty()) {
            (false, false) => format!("{} {}", v, m),
            (false, true) => v.to_string(),
            (true, false) => m.to_string(),
            (true, true) => "USB Flash Drive".to_string(),
        };
        format!("💾 {} ({:.1} GB) — {}", self.device.display(), self.size_gb, desc)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct IsoInfo {
    pub path: PathBuf,
    pub file_name: String,
    pub size_gb: f64,
    pub is_windows: bool,
    pub os_label: String,
    pub has_win11_hint: bool,
}

#[derive(Clone, Debug)]
pub struct BurnProgress {
    pub phase: String,
    pub fraction: f32, // 0.0 .. 1.0
    pub bytes_written: u64,
    pub total_bytes: u64,
    pub speed_mb_s: f64,
    pub eta_secs: u64,
    pub is_done: bool,
    pub error: Option<String>,
}

impl BurnProgress {
    pub fn new_step(phase: &str, fraction: f32) -> Self {
        Self {
            phase: phase.to_string(),
            fraction: fraction.clamp(0.0, 1.0),
            bytes_written: 0,
            total_bytes: 0,
            speed_mb_s: 0.0,
            eta_secs: 0,
            is_done: false,
            error: None,
        }
    }

    pub fn done(phase: &str) -> Self {
        Self {
            phase: phase.to_string(),
            fraction: 1.0,
            bytes_written: 0,
            total_bytes: 0,
            speed_mb_s: 0.0,
            eta_secs: 0,
            is_done: true,
            error: None,
        }
    }

    pub fn failed(phase: &str, err: &str) -> Self {
        Self {
            phase: phase.to_string(),
            fraction: 0.0,
            bytes_written: 0,
            total_bytes: 0,
            speed_mb_s: 0.0,
            eta_secs: 0,
            is_done: true,
            error: Some(err.to_string()),
        }
    }
}

// ============================================================================
// 2. Hardware Safety, Privilege Checks & USB Drive Discovery
// ============================================================================

pub fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

pub fn ensure_root() -> Result<()> {
    if !is_root() {
        bail!("Root privileges required! Please run with sudo or pkexec.");
    }
    Ok(())
}

fn read_sys(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default().trim().to_string()
}

pub fn list_usb_drives() -> Result<Vec<UsbDrive>> {
    let mut drives = Vec::new();
    let sys_block = Path::new("/sys/block");
    if !sys_block.exists() {
        return Ok(drives);
    }

    // Read active system mounts to safeguard internal disks
    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();

    for entry in fs::read_dir(sys_block)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("sd") {
            continue;
        }

        // Must be marked as removable in /sys/block/<name>/removable
        if read_sys(&entry.path().join("removable")) != "1" {
            continue;
        }

        let sectors: u64 = read_sys(&entry.path().join("size")).parse().unwrap_or(0);
        if sectors == 0 {
            continue;
        }

        let dev_path = PathBuf::from(format!("/dev/{}", name));
        let dev_str = dev_path.to_string_lossy();

        // Safety verification: device must not hold critical root mounts
        let mut is_critical_system = false;
        for line in mounts.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[0].starts_with(dev_str.as_ref()) {
                if ["/", "/boot", "/boot/efi", "/home", "/var"].contains(&parts[1]) {
                    is_critical_system = true;
                    break;
                }
            }
        }
        if is_critical_system {
            continue;
        }

        let size_gb = (sectors * 512) as f64 / 1_000_000_000.0;
        let vendor = read_sys(&entry.path().join("device/vendor"));
        let model = read_sys(&entry.path().join("device/model"));

        drives.push(UsbDrive {
            device: dev_path,
            name,
            size_gb,
            vendor,
            model,
        });
    }

    drives.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(drives)
}

pub fn verify_safe_usb(dev: &Path) -> Result<()> {
    if !dev.exists() {
        bail!("Device {:?} does not exist!", dev);
    }
    let name = dev.file_name().context("Invalid device path")?.to_string_lossy();
    let rem_path = PathBuf::from(format!("/sys/block/{}/removable", name));

    if read_sys(&rem_path) != "1" {
        bail!("SAFETY BLOCK: {:?} is NOT marked as a removable USB drive!", dev);
    }

    let mounts = fs::read_to_string("/proc/mounts").unwrap_or_default();
    let prefix = dev.to_string_lossy();
    for line in mounts.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && parts[0].starts_with(prefix.as_ref()) {
            if ["/", "/boot", "/boot/efi", "/home", "/var"].contains(&parts[1]) {
                bail!("SAFETY BLOCK: {:?} contains critical system mount ({})!", dev, parts[1]);
            }
        }
    }
    Ok(())
}

pub fn unmount_all_partitions(dev: &Path) {
    let prefix = dev.to_string_lossy();
    if let Ok(mounts) = fs::read_to_string("/proc/mounts") {
        for line in mounts.lines() {
            if let Some(part) = line.split_whitespace().next() {
                if part.starts_with(prefix.as_ref()) {
                    let _ = Command::new("umount")
                        .args(["-l", part])
                        .stderr(Stdio::null())
                        .status();
                }
            }
        }
    }
}

// ============================================================================
// 3. ISO Analysis & Inspection Engine
// ============================================================================

pub fn inspect_iso(iso_path: &Path) -> Result<IsoInfo> {
    if !iso_path.exists() {
        bail!("ISO file does not exist: {:?}", iso_path);
    }

    let meta = fs::metadata(iso_path)?;
    let size_gb = meta.len() as f64 / 1_000_000_000.0;
    let file_name = iso_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "image.iso".to_string());

    let file_name_lower = file_name.to_lowercase();
    let has_win11_hint = file_name_lower.contains("win11") || file_name_lower.contains("windows11");

    // Check Windows ISO signatures:
    // Method A: Check using 7z without requiring root/mount
    let mut is_windows = false;
    if let Ok(output) = Command::new("7z")
        .args(["l", "-ba"])
        .arg(iso_path)
        .arg("sources/install.wim")
        .arg("sources/install.esd")
        .arg("sources/boot.wim")
        .output()
    {
        let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
        if stdout.contains("install.wim") || stdout.contains("install.esd") || stdout.contains("boot.wim") {
            is_windows = true;
        }
    }

    // Method B: Fallback check using MountGuard if running as root
    if !is_windows && is_root() {
        if let Ok(win) = detect_windows_iso(iso_path) {
            is_windows = win;
        }
    }

    // Method C: Filename heuristic fallback
    if !is_windows && (file_name_lower.contains("windows") || file_name_lower.contains("win10") || has_win11_hint) {
        is_windows = true;
    }

    let os_label = if is_windows {
        "🪟 Windows 10/11 UEFI Dual-Partition (FAT32 Boot + NTFS Data)".to_string()
    } else {
        "🐧 Linux / BSD Hybrid Image (Raw Stream O_SYNC)".to_string()
    };

    Ok(IsoInfo {
        path: iso_path.to_path_buf(),
        file_name,
        size_gb,
        is_windows,
        os_label,
        has_win11_hint,
    })
}

pub fn detect_windows_iso(iso_path: &Path) -> Result<bool> {
    let iso_mount = MountGuard::new("probe", iso_path, MountKind::IsoLoop)?;
    let has_wim = iso_mount.path().join("sources/install.wim").exists()
        || iso_mount.path().join("sources/install.esd").exists()
        || iso_mount.path().join("sources/boot.wim").exists();
    Ok(has_wim)
}

// ============================================================================
// 4. MountGuard RAII Wrapper
// ============================================================================

pub enum MountKind {
    IsoLoop,
    Vfat,
    Ntfs,
}

pub struct MountGuard {
    mount_point: PathBuf,
}

impl MountGuard {
    pub fn new(prefix: &str, source: &Path, kind: MountKind) -> Result<Self> {
        let pid = std::process::id();
        let mount_point = PathBuf::from(format!("/tmp/nextburn_{}_{}", prefix, pid));
        fs::create_dir_all(&mount_point)?;

        let success = match kind {
            MountKind::IsoLoop => Command::new("mount")
                .args(["-o", "ro,loop"])
                .arg(source)
                .arg(&mount_point)
                .status()?
                .success(),
            MountKind::Vfat => Command::new("mount")
                .args(["-t", "vfat"])
                .arg(source)
                .arg(&mount_point)
                .status()?
                .success(),
            MountKind::Ntfs => {
                let _ = Command::new("modprobe").arg("ntfs3").status();
                let try_ntfs3 = Command::new("mount")
                    .args(["-t", "ntfs3"])
                    .arg(source)
                    .arg(&mount_point)
                    .stderr(Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);

                if try_ntfs3 {
                    true
                } else {
                    Command::new("mount")
                        .args(["-t", "ntfs-3g"])
                        .arg(source)
                        .arg(&mount_point)
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false)
                }
            }
        };

        if !success {
            let _ = fs::remove_dir(&mount_point);
            bail!("Failed to mount {:?} at {:?}", source, mount_point);
        }

        Ok(Self { mount_point })
    }

    pub fn path(&self) -> &Path {
        &self.mount_point
    }
}

impl Drop for MountGuard {
    fn drop(&mut self) {
        let _ = Command::new("sync").status();
        let ok = Command::new("umount")
            .arg(&self.mount_point)
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !ok {
            let _ = Command::new("umount")
                .args(["-l", &self.mount_point.to_string_lossy()])
                .stderr(Stdio::null())
                .status();
        }
        let _ = fs::remove_dir(&self.mount_point);
    }
}

// ============================================================================
// 5. Raw Hybrid Linux Burner Engine
// ============================================================================

pub fn burn_raw_iso(
    iso_path: &Path,
    dev_path: &Path,
    tx: Sender<BurnProgress>,
) -> Result<()> {
    ensure_root()?;
    verify_safe_usb(dev_path)?;
    unmount_all_partitions(dev_path);

    let mut iso = File::open(iso_path).context("Cannot open ISO file")?;
    let total_bytes = iso.metadata()?.len();

    let mut dev = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_SYNC)
        .open(dev_path)
        .context("Failed to open target USB device for direct writing")?;

    let mut buf = vec![0u8; IO_CHUNK_SIZE];
    let mut written = 0u64;

    let start_time = Instant::now();
    let mut last_emit = Instant::now();

    loop {
        let n = iso.read(&mut buf)?;
        if n == 0 {
            break;
        }
        dev.write_all(&buf[..n])?;
        written += n as u64;

        if last_emit.elapsed() >= Duration::from_millis(150) || written == total_bytes {
            let elapsed_sec = start_time.elapsed().as_secs_f64().max(0.001);
            let speed_mb_s = (written as f64 / 1_000_000.0) / elapsed_sec;
            let remaining_bytes = total_bytes.saturating_sub(written);
            let eta_secs = if speed_mb_s > 0.05 {
                (remaining_bytes as f64 / (speed_mb_s * 1_000_000.0)) as u64
            } else {
                0
            };
            let fraction = (written as f32 / total_bytes.max(1) as f32).clamp(0.0, 0.98);

            tx.send(BurnProgress {
                phase: "Writing Raw Hybrid ISO stream (O_SYNC)...".to_string(),
                fraction,
                bytes_written: written,
                total_bytes,
                speed_mb_s,
                eta_secs,
                is_done: false,
                error: None,
            })
            .ok();

            last_emit = Instant::now();
        }
    }

    tx.send(BurnProgress {
        phase: "Flushing hardware cache & syncing block devices...".to_string(),
        fraction: 0.99,
        bytes_written: written,
        total_bytes,
        speed_mb_s: 0.0,
        eta_secs: 0,
        is_done: false,
        error: None,
    })
    .ok();

    dev.sync_all()?;
    let _ = Command::new("partprobe").arg(dev_path).status();

    tx.send(BurnProgress::done("✅ Burn Complete! Safe to remove USB drive."))
        .ok();
    Ok(())
}

// ============================================================================
// 6. Windows 10/11 UEFI Dual-Partition Burner Engine
// ============================================================================

pub fn burn_windows_iso(
    iso_path: &Path,
    dev_path: &Path,
    label: &str,
    win11_bypass: bool,
    tx: Sender<BurnProgress>,
) -> Result<()> {
    ensure_root()?;
    verify_safe_usb(dev_path)?;
    unmount_all_partitions(dev_path);

    // [1/5] Mount ISO
    tx.send(BurnProgress::new_step("-> [1/5] Mounting Windows ISO image...", 0.05))
        .ok();
    let iso_mount = MountGuard::new("iso", iso_path, MountKind::IsoLoop)?;

    // [2/5] Partitioning GPT
    tx.send(BurnProgress::new_step(
        &format!("-> [2/5] Creating GPT partition table on {:?}...", dev_path),
        0.10,
    ))
    .ok();
    let _ = Command::new("wipefs")
        .args(["-a", &dev_path.to_string_lossy()])
        .status();

    let sfdisk_script = "label: gpt\n\
size=1536M, type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, name=\"UEFI_BOOT\"\n\
type=EBD0A0A2-B9E5-4433-87C0-68B6B72699C7, name=\"WIN_INSTALL\"\n";

    let mut sfdisk = Command::new("sfdisk")
        .arg("--force")
        .arg(dev_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("Failed to execute sfdisk")?;

    sfdisk
        .stdin
        .as_mut()
        .context("Failed to open sfdisk stdin")?
        .write_all(sfdisk_script.as_bytes())?;

    if !sfdisk.wait()?.success() {
        bail!("sfdisk failed to partition {:?}", dev_path);
    }

    sync_kernel_partitions(dev_path)?;

    let part1 = partition_path(dev_path, 1);
    let part2 = partition_path(dev_path, 2);
    wait_for_block_nodes(&[&part1, &part2])?;

    // [3/5] Formatting Partitions
    tx.send(BurnProgress::new_step(
        "-> [3/5] Formatting partitions (FAT32 Boot + NTFS Data)...",
        0.18,
    ))
    .ok();
    let clean_label: String = label.chars().take(11).collect();

    let _ = Command::new("wipefs").args(["-a", &part1.to_string_lossy()]).status();
    let _ = Command::new("wipefs").args(["-a", &part2.to_string_lossy()]).status();

    run_cmd("mkfs.vfat", &["-F", "32", "-n", "UEFI_BOOT", &part1.to_string_lossy()])?;
    run_cmd("mkfs.ntfs", &["-Q", "-F", "-L", &clean_label, &part2.to_string_lossy()])?;

    let _ = Command::new("udevadm").args(["settle", "--timeout=5"]).status();
    unmount_all_partitions(dev_path);

    // [4/5] Copying Files
    tx.send(BurnProgress::new_step(
        "-> [4/5] Preparing mounts and calculating copy subsets...",
        0.22,
    ))
    .ok();
    let fat_mount = MountGuard::new("fat32", &part1, MountKind::Vfat)?;
    let ntfs_mount = MountGuard::new("ntfs", &part2, MountKind::Ntfs)?;

    let boot_total = calc_boot_subset_size(iso_mount.path())?;
    let full_total = calc_dir_size(iso_mount.path())?;
    let grand_total = boot_total + full_total;

    let start_time = Instant::now();
    let mut copied_bytes = 0u64;

    // A. Copy UEFI Boot Subset (FAT32)
    copy_boot_subset(
        iso_mount.path(),
        fat_mount.path(),
        grand_total,
        &mut copied_bytes,
        start_time,
        &tx,
    )?;

    // B. Copy Full Windows Installation Image (NTFS)
    copy_tree_recursive(
        iso_mount.path(),
        ntfs_mount.path(),
        grand_total,
        &mut copied_bytes,
        start_time,
        &tx,
    )?;

    // C. Inject Windows 11 Bypass if enabled
    if win11_bypass {
        tx.send(BurnProgress::new_step(
            "Injecting Windows 11 TPM/RAM Bypass (autounattend.xml)...",
            0.96,
        ))
        .ok();
        write_win11_bypass(fat_mount.path())?;
        write_win11_bypass(ntfs_mount.path())?;
    }

    // [5/5] Flushing and Safe Unmount
    tx.send(BurnProgress::new_step(
        "-> [5/5] Flushing filesystems and unmounting safely...",
        0.98,
    ))
    .ok();
    let _ = Command::new("sync").status();

    tx.send(BurnProgress::done("✅ Windows 10/11 UEFI USB Ready! Safe to remove drive."))
        .ok();
    Ok(())
}

// ============================================================================
// 7. Controlled Copy Engine & Partition Helpers
// ============================================================================

fn copy_boot_subset(
    iso_root: &Path,
    fat_root: &Path,
    grand_total: u64,
    copied: &mut u64,
    start_time: Instant,
    tx: &Sender<BurnProgress>,
) -> Result<()> {
    for entry in fs::read_dir(iso_root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_lower = name.to_string_lossy().to_lowercase();
        let src_path = entry.path();
        let dst_path = fat_root.join(&name);

        if name_lower == "sources" && src_path.is_dir() {
            fs::create_dir_all(&dst_path)?;
            let boot_wim = src_path.join("boot.wim");
            if boot_wim.exists() {
                copy_file_synced(
                    &boot_wim,
                    &dst_path.join("boot.wim"),
                    grand_total,
                    copied,
                    start_time,
                    "Copying UEFI Boot files (FAT32)...",
                    tx,
                )?;
            }
        } else if src_path.is_dir() {
            copy_tree_inner(
                &src_path,
                &dst_path,
                grand_total,
                copied,
                start_time,
                "Copying UEFI Boot support files...",
                tx,
            )?;
        } else {
            copy_file_synced(
                &src_path,
                &dst_path,
                grand_total,
                copied,
                start_time,
                "Copying UEFI Boot files (FAT32)...",
                tx,
            )?;
        }
    }
    Ok(())
}

fn copy_tree_recursive(
    src: &Path,
    dst: &Path,
    grand_total: u64,
    copied: &mut u64,
    start_time: Instant,
    tx: &Sender<BurnProgress>,
) -> Result<()> {
    copy_tree_inner(
        src,
        dst,
        grand_total,
        copied,
        start_time,
        "Copying Windows Installation image (NTFS)...",
        tx,
    )
}

fn copy_tree_inner(
    src: &Path,
    dst: &Path,
    grand_total: u64,
    copied: &mut u64,
    start_time: Instant,
    phase_label: &str,
    tx: &Sender<BurnProgress>,
) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_tree_inner(
                &src_path,
                &dst_path,
                grand_total,
                copied,
                start_time,
                phase_label,
                tx,
            )?;
        } else {
            copy_file_synced(
                &src_path,
                &dst_path,
                grand_total,
                copied,
                start_time,
                phase_label,
                tx,
            )?;
        }
    }
    Ok(())
}

fn copy_file_synced(
    src: &Path,
    dst: &Path,
    grand_total: u64,
    copied: &mut u64,
    start_time: Instant,
    phase_label: &str,
    tx: &Sender<BurnProgress>,
) -> Result<()> {
    let mut in_file = File::open(src)?;
    let file_len = in_file.metadata()?.len();
    let mut out_file = File::create(dst)?;
    let mut buf = vec![0u8; IO_CHUNK_SIZE];
    let mut since_last_sync = 0u64;
    let mut last_emit = Instant::now();

    loop {
        let n = in_file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out_file.write_all(&buf[..n])?;
        *copied += n as u64;
        since_last_sync += n as u64;

        if last_emit.elapsed() >= Duration::from_millis(150) {
            let elapsed_sec = start_time.elapsed().as_secs_f64().max(0.001);
            let speed_mb_s = (*copied as f64 / 1_000_000.0) / elapsed_sec;
            let remaining = grand_total.saturating_sub(*copied);
            let eta_secs = if speed_mb_s > 0.05 {
                (remaining as f64 / (speed_mb_s * 1_000_000.0)) as u64
            } else {
                0
            };
            // Reserve 0.20 to 0.95 for the copy phase
            let copy_fraction = 0.20 + 0.75 * (*copied as f32 / grand_total.max(1) as f32);

            tx.send(BurnProgress {
                phase: phase_label.to_string(),
                fraction: copy_fraction.clamp(0.20, 0.95),
                bytes_written: *copied,
                total_bytes: grand_total,
                speed_mb_s,
                eta_secs,
                is_done: false,
                error: None,
            })
            .ok();

            last_emit = Instant::now();
        }

        if since_last_sync >= SYNC_INTERVAL {
            out_file.sync_data()?;
            since_last_sync = 0;
        }
    }

    if file_len >= SYNC_INTERVAL {
        out_file.sync_data()?;
    }
    Ok(())
}

fn calc_boot_subset_size(iso_root: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(iso_root)? {
        let entry = entry?;
        let name_lower = entry.file_name().to_string_lossy().to_lowercase();
        let path = entry.path();

        if name_lower == "sources" && path.is_dir() {
            let boot_wim = path.join("boot.wim");
            if let Ok(meta) = boot_wim.metadata() {
                total += meta.len();
            }
        } else if path.is_dir() {
            total += calc_dir_size(&path)?;
        } else if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    Ok(total)
}

fn calc_dir_size(dir: &Path) -> Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            total += calc_dir_size(&path)?;
        } else {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

pub fn write_win11_bypass(root: &Path) -> Result<()> {
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend">
<settings pass="windowsPE">
<component name="Microsoft-Windows-Setup" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
<RunSynchronous>
<RunSynchronousCommand wcm:action="add" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
<Order>1</Order>
<Path>reg add HKLM\SYSTEM\Setup\LabConfig /v BypassTPMCheck /t REG_DWORD /d 1 /f</Path>
</RunSynchronousCommand>
<RunSynchronousCommand wcm:action="add" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
<Order>2</Order>
<Path>reg add HKLM\SYSTEM\Setup\LabConfig /v BypassSecureBootCheck /t REG_DWORD /d 1 /f</Path>
</RunSynchronousCommand>
<RunSynchronousCommand wcm:action="add" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
<Order>3</Order>
<Path>reg add HKLM\SYSTEM\Setup\LabConfig /v BypassRAMCheck /t REG_DWORD /d 1 /f</Path>
</RunSynchronousCommand>
<RunSynchronousCommand wcm:action="add" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
<Order>4</Order>
<Path>reg add HKLM\SYSTEM\Setup\LabConfig /v BypassCPUCheck /t REG_DWORD /d 1 /f</Path>
</RunSynchronousCommand>
<RunSynchronousCommand wcm:action="add" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
<Order>5</Order>
<Path>reg add HKLM\SYSTEM\Setup\LabConfig /v BypassStorageCheck /t REG_DWORD /d 1 /f</Path>
</RunSynchronousCommand>
</RunSynchronous>
</component>
</settings>
</unattend>"#;
    fs::write(root.join("autounattend.xml"), xml)?;
    Ok(())
}

fn partition_path(dev: &Path, num: u8) -> PathBuf {
    let s = dev.to_string_lossy();
    if s.chars().last().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        PathBuf::from(format!("{}p{}", s, num))
    } else {
        PathBuf::from(format!("{}{}", s, num))
    }
}

fn sync_kernel_partitions(dev: &Path) -> Result<()> {
    let _ = Command::new("blockdev").args(["--rereadpt", &dev.to_string_lossy()]).status();
    let _ = Command::new("partprobe").arg(dev).status();
    let _ = Command::new("udevadm").args(["settle", "--timeout=5"]).status();
    Ok(())
}

fn wait_for_block_nodes(nodes: &[&Path]) -> Result<()> {
    for _ in 0..30 {
        if nodes.iter().all(|n| n.exists()) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    }
    bail!("Timed out waiting for kernel partition nodes: {:?}", nodes)
}

fn run_cmd(bin: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(bin)
        .args(args)
        .stdout(Stdio::null())
        .status()
        .with_context(|| format!("Failed to launch {}", bin))?;
    if !status.success() {
        bail!("Command {} {:?} exited with error", bin, args);
    }
    Ok(())
}
