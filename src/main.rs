// ============================================================================
// main.rs — NextBurn (GUI & CLI Production-Grade USB Burner)
// ============================================================================

mod engine;
mod gui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "nextburn",
    version,
    about = "Production-Grade Linux & Windows 10/11 UEFI USB Creator (GUI & CLI)"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Launch the NextBurn GUI interface
    Gui,
    /// Scan and list removable safe USB drives
    List,
    /// Burn an ISO image to target USB device
    Burn {
        /// Path to the ISO image file
        #[arg(short, long)]
        iso: PathBuf,

        /// Target USB block device (e.g. /dev/sdb)
        #[arg(short, long)]
        device: PathBuf,

        /// Force Windows UEFI mode (auto-detected if omitted)
        #[arg(short, long)]
        windows: bool,

        /// Volume Label for the drive
        #[arg(short, long, default_value = "NEXTBURN")]
        label: String,

        /// Bypass Windows 11 TPM 2.0, SecureBoot & RAM checks (autounattend.xml)
        #[arg(long)]
        win11_bypass: bool,
    },
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 1 {
        gui::run_gui();
        return Ok(());
    }

    let cli = Cli::parse();
    match cli.command {
        None | Some(Commands::Gui) => {
            gui::run_gui();
        }
        Some(Commands::List) => {
            let drives = engine::list_usb_drives()?;
            println!("{:<14} {:<10} {:<15} {:<22}", "DEVICE", "SIZE", "VENDOR", "MODEL");
            println!("{}", "-".repeat(65));
            if drives.is_empty() {
                println!("No removable USB drives detected.");
            } else {
                for d in drives {
                    println!(
                        "{:<14} {:<10} {:<15} {:<22}",
                        d.device.display(),
                        format!("{:.1} GB", d.size_gb),
                        d.vendor.trim(),
                        d.model.trim()
                    );
                }
            }
        }
        Some(Commands::Burn {
            iso,
            device,
            windows,
            label,
            win11_bypass,
        }) => {
            engine::ensure_root()?;
            engine::verify_safe_usb(&device)?;

            let is_win = windows || engine::detect_windows_iso(&iso).unwrap_or(false);
            let (tx, rx): (std::sync::mpsc::Sender<engine::BurnProgress>, _) = std::sync::mpsc::channel();

            let handle = std::thread::spawn(move || {
                let mut last_phase = String::new();
                while let Ok(prog) = rx.recv() {
                    if prog.phase != last_phase {
                        println!("\n📌 {}", prog.phase);
                        last_phase = prog.phase.clone();
                    }
                    if prog.total_bytes > 0 {
                        print!(
                            "\r[{:>3.0}%] Transferred: {:.2} GB / {:.2} GB  •  Speed: {:.1} MB/s  •  ETA: {}s",
                            prog.fraction * 100.0,
                            prog.bytes_written as f64 / 1_000_000_000.0,
                            prog.total_bytes as f64 / 1_000_000_000.0,
                            prog.speed_mb_s,
                            prog.eta_secs
                        );
                        std::io::stdout().flush().ok();
                    }
                    if prog.is_done {
                        if let Some(err) = prog.error {
                            eprintln!("\n❌ Error: {}", err);
                        } else {
                            println!("\n✅ Done!");
                        }
                        break;
                    }
                }
            });

            if is_win {
                println!("🪟 Mode: Windows 10/11 UEFI Dual-Partition (FAT32 Boot + NTFS Data)");
                engine::burn_windows_iso(&iso, &device, &label, win11_bypass, tx)?;
            } else {
                println!("🐧 Mode: Linux Hybrid Image (O_SYNC Direct Write)");
                engine::burn_raw_iso(&iso, &device, tx)?;
            }

            handle.join().ok();
        }
    }
    Ok(())
}
