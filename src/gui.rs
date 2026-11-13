// ============================================================================
// gui.rs — NextBurn Rufus/Etcher-Style Modern UEFI USB Burner GUI (wxDragon)
// ============================================================================

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use wxdragon::dialogs::file_dialog::{FileDialog, FileDialogStyle};
use wxdragon::dialogs::message_dialog::{MessageDialog, MessageDialogStyle};
use wxdragon::prelude::*;
use wxdragon::timer::Timer;

#[path = "engine.rs"]
mod engine;
use engine::{BurnProgress, IsoInfo, UsbDrive};

// ============================================================================
// GUI State
// ============================================================================

#[derive(Default)]
struct SharedGuiState {
    drives: Vec<UsbDrive>,
    selected_drive_idx: Option<usize>,
    selected_iso: Option<IsoInfo>,
    is_burning: bool,
}

// ============================================================================
// GUI Entry Point
// ============================================================================

pub fn run_gui() {
    SystemOptions::set_option_by_int("msw.no-manifest-check", 1);

    let _ = wxdragon::main(|_| {
        let frame = Frame::builder()
            .with_title("NextBurn — Modern UEFI USB Creator")
            .with_size(Size::new(640, 730))
            .build();

        let panel = Panel::builder(&frame).build();
        let main_sizer = BoxSizer::builder(Orientation::Vertical).build();

        let shared_state: Arc<Mutex<SharedGuiState>> = Arc::new(Mutex::new(SharedGuiState::default()));

        // ── 1. Top Header Banner ─────────────────────────────────────────────
        let title = StaticText::builder(&panel)
            .with_label("NextBurn")
            .build();

        if let Some(f) = Font::builder()
            .with_point_size(18)
            .with_weight(FontWeight::Bold)
            .build()
        {
            title.set_font(&f);
        }

        let is_root = engine::is_root();
        let subtitle_str = if is_root {
            "Production-Grade Linux && Windows 10/11 UEFI USB Creator  •  🛡️ Root Active"
        } else {
            "Production-Grade Linux && Windows 10/11 UEFI USB Creator  •  ⚠️ Non-Root Mode"
        };

        let subtitle = StaticText::builder(&panel)
            .with_label(subtitle_str)
            .build();

        main_sizer.add(&title, 0, SizerFlag::Top | SizerFlag::Left | SizerFlag::Right, 14);
        main_sizer.add(&subtitle, 0, SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);

        // ── 2. Step 1: Target USB Drive Section ──────────────────────────────
        let drive_box_sizer = StaticBoxSizerBuilder::new_with_label(
            Orientation::Vertical,
            &panel,
            " 1. Target USB Device ",
        )
        .build();

        let drive_row = BoxSizer::builder(Orientation::Horizontal).build();
        let choice_drives = Choice::builder(&panel).build();
        let btn_rescan = Button::builder(&panel)
            .with_label("  🔄 Rescan  ")
            .build();

        drive_row.add(&choice_drives, 1, SizerFlag::All | SizerFlag::AlignCenterVertical, 4);
        drive_row.add(&btn_rescan, 0, SizerFlag::All | SizerFlag::AlignCenterVertical, 4);
        drive_box_sizer.add_sizer(&drive_row, 0, SizerFlag::Expand | SizerFlag::All, 4);

        let lbl_drive_details = StaticText::builder(&panel)
            .with_label("Scanning for removable USB drives...")
            .build();
        drive_box_sizer.add(&lbl_drive_details, 0, SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 8);

        main_sizer.add_sizer(&drive_box_sizer, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);

        // ── 3. Step 2: Boot Selection / ISO Image Section ────────────────────
        let iso_box_sizer = StaticBoxSizerBuilder::new_with_label(
            Orientation::Vertical,
            &panel,
            " 2. Boot Selection (ISO Image) ",
        )
        .build();

        let iso_row = BoxSizer::builder(Orientation::Horizontal).build();
        let txt_iso_path = TextCtrl::builder(&panel)
            .with_value("No ISO image selected — click Browse...")
            .with_style(TextCtrlStyle::ReadOnly)
            .build();

        let btn_browse_iso = Button::builder(&panel)
            .with_label("  Browse...  ")
            .build();

        iso_row.add(&txt_iso_path, 1, SizerFlag::All | SizerFlag::AlignCenterVertical, 4);
        iso_row.add(&btn_browse_iso, 0, SizerFlag::All | SizerFlag::AlignCenterVertical, 4);
        iso_box_sizer.add_sizer(&iso_row, 0, SizerFlag::Expand | SizerFlag::All, 4);

        let lbl_iso_details = StaticText::builder(&panel)
            .with_label("Supports Linux Hybrid ISOs and Windows 10/11 UEFI install images.")
            .build();
        iso_box_sizer.add(&lbl_iso_details, 0, SizerFlag::Left | SizerFlag::Right, 8);

        let lbl_detected_mode = StaticText::builder(&panel)
            .with_label("Detected Format: (waiting for ISO)")
            .build();
        if let Some(fb) = Font::builder().with_weight(FontWeight::Bold).build() {
            lbl_detected_mode.set_font(&fb);
        }
        iso_box_sizer.add(&lbl_detected_mode, 0, SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 8);

        main_sizer.add_sizer(&iso_box_sizer, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);

        // ── 4. Step 3: Options & Windows 11 Bypasses ──────────────────────────
        let opt_box_sizer = StaticBoxSizerBuilder::new_with_label(
            Orientation::Vertical,
            &panel,
            " 3. Format && Target Options ",
        )
        .build();

        let label_row = BoxSizer::builder(Orientation::Horizontal).build();
        let lbl_vol_caption = StaticText::builder(&panel)
            .with_label("Volume Label:")
            .build();
        let txt_volume_label = TextCtrl::builder(&panel)
            .with_value("NEXTBURN")
            .build();
        let lbl_target_sys = StaticText::builder(&panel)
            .with_label("Target System: UEFI (Dual-Partition)")
            .build();

        label_row.add(&lbl_vol_caption, 0, SizerFlag::All | SizerFlag::AlignCenterVertical, 4);
        label_row.add(&txt_volume_label, 0, SizerFlag::All | SizerFlag::AlignCenterVertical, 4);
        label_row.add_stretch_spacer(1);
        label_row.add(&lbl_target_sys, 0, SizerFlag::All | SizerFlag::AlignCenterVertical, 4);
        opt_box_sizer.add_sizer(&label_row, 0, SizerFlag::Expand | SizerFlag::All, 4);

        let chk_win11_bypass = CheckBox::builder(&panel)
            .with_label("Bypass Windows 11 TPM 2.0, SecureBoot && 4GB RAM checks (autounattend.xml)")
            .build();
        chk_win11_bypass.set_value(true);
        opt_box_sizer.add(&chk_win11_bypass, 0, SizerFlag::All, 6);

        main_sizer.add_sizer(&opt_box_sizer, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);

        // ── 5. Step 4: Status & Progress Gauge ───────────────────────────────
        let prog_box_sizer = StaticBoxSizerBuilder::new_with_label(
            Orientation::Vertical,
            &panel,
            " 4. Progress && Status ",
        )
        .build();

        let gauge = Gauge::builder(&panel)
            .build();
        gauge.set_range(1000);
        gauge.set_value(0);
        prog_box_sizer.add(&gauge, 0, SizerFlag::Expand | SizerFlag::All, 6);

        let lbl_phase = StaticText::builder(&panel)
            .with_label("Status: Ready to burn. Select USB device and ISO.")
            .build();
        if let Some(fb) = Font::builder().with_weight(FontWeight::Bold).build() {
            lbl_phase.set_font(&fb);
        }
        prog_box_sizer.add(&lbl_phase, 0, SizerFlag::Left | SizerFlag::Right | SizerFlag::Top, 6);

        let lbl_stats = StaticText::builder(&panel)
            .with_label("Speed: 0.0 MB/s  •  Transferred: 0 GB / 0 GB  •  ETA: --:--")
            .build();
        prog_box_sizer.add(&lbl_stats, 0, SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 6);

        main_sizer.add_sizer(&prog_box_sizer, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);

        // ── 6. Step 5: Action Buttons (START / Close) ────────────────────────
        let action_row = BoxSizer::builder(Orientation::Horizontal).build();
        let btn_burn = Button::builder(&panel)
            .with_label("    🔥 START / צרוב כעת    ")
            .build();

        if let Some(f_btn) = Font::builder()
            .with_point_size(12)
            .with_weight(FontWeight::Bold)
            .build()
        {
            btn_burn.set_font(&f_btn);
        }

        let btn_close = Button::builder(&panel)
            .with_label("  Close / יציאה  ")
            .build();

        action_row.add_stretch_spacer(1);
        action_row.add(&btn_burn, 0, SizerFlag::All, 6);
        action_row.add(&btn_close, 0, SizerFlag::All, 6);

        main_sizer.add_sizer(&action_row, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);

        panel.set_sizer(main_sizer, true);
        panel.layout();

        // ── 7. Timer & Channel for Async Progress Sync ───────────────────────
        let channel_receiver: Arc<Mutex<Option<Receiver<BurnProgress>>>> = Arc::new(Mutex::new(None));

        let timer = Timer::new(&frame);
        timer.start(60, false);

        // ── Helper: Refresh USB Drives List ──────────────────────────────────
        let refresh_drives = {
            let choice_drives = choice_drives;
            let lbl_drive_details = lbl_drive_details;
            let shared_state = Arc::clone(&shared_state);

            move || {
                choice_drives.clear();
                let drives = engine::list_usb_drives().unwrap_or_default();
                let mut state = shared_state.lock().unwrap();
                state.drives = drives.clone();

                if drives.is_empty() {
                    choice_drives.append("No removable USB drive detected");
                    choice_drives.set_selection(0);
                    state.selected_drive_idx = None;
                    lbl_drive_details.set_label("⚠️ No removable USB drives detected. Plug in a USB flash drive and click Rescan.");
                } else {
                    for d in &drives {
                        choice_drives.append(&d.display_name());
                    }
                    choice_drives.set_selection(0);
                    state.selected_drive_idx = Some(0);
                    let d = &drives[0];
                    lbl_drive_details.set_label(&format!(
                        "Selected: {} ({:.1} GB) — Model: {} {}",
                        d.device.display(),
                        d.size_gb,
                        d.vendor.trim(),
                        d.model.trim()
                    ));
                }
            }
        };

        // Initial scan on launch
        refresh_drives();

        // ── Event: Rescan Button Clicked ─────────────────────────────────────
        btn_rescan.on_click({
            let refresh_drives = refresh_drives.clone();
            move |_| {
                refresh_drives();
            }
        });

        // ── Event: USB Drive Selection Changed ───────────────────────────────
        choice_drives.on_selection_changed({
            let choice_drives = choice_drives;
            let lbl_drive_details = lbl_drive_details;
            let shared_state = Arc::clone(&shared_state);

            move |_| {
                if let Some(sel) = choice_drives.get_selection() {
                    let mut state = shared_state.lock().unwrap();
                    let idx = sel as usize;
                    if idx < state.drives.len() {
                        state.selected_drive_idx = Some(idx);
                        let d = &state.drives[idx];
                        lbl_drive_details.set_label(&format!(
                            "Selected: {} ({:.1} GB) — Model: {} {}",
                            d.device.display(),
                            d.size_gb,
                            d.vendor.trim(),
                            d.model.trim()
                        ));
                    }
                }
            }
        });

        // ── Event: Browse ISO Button Clicked ─────────────────────────────────
        btn_browse_iso.on_click({
            let panel_ref = panel;
            let txt_iso_path = txt_iso_path;
            let lbl_iso_details = lbl_iso_details;
            let lbl_detected_mode = lbl_detected_mode;
            let chk_win11_bypass = chk_win11_bypass;
            let shared_state = Arc::clone(&shared_state);

            move |_| {
                let file_dialog = FileDialog::builder(&panel_ref)
                    .with_message("Select Windows or Linux ISO Image")
                    .with_wildcard("ISO Disk Images (*.iso)|*.iso|All Files (*.*)|*.*")
                    .with_style(FileDialogStyle::Open)
                    .build();

                if file_dialog.show_modal() == ID_OK {
                    if let Some(path_str) = file_dialog.get_path() {
                        let path = PathBuf::from(&path_str);
                        match engine::inspect_iso(&path) {
                            Ok(info) => {
                                txt_iso_path.set_value(&path_str);
                                lbl_iso_details.set_label(&format!(
                                    "File: {}  •  Size: {:.2} GB",
                                    info.file_name,
                                    info.size_gb
                                ));
                                lbl_detected_mode.set_label(&format!("Detected Format: {}", info.os_label));

                                if info.is_windows {
                                    chk_win11_bypass.enable(true);
                                    chk_win11_bypass.set_value(true);
                                } else {
                                    chk_win11_bypass.set_value(false);
                                    chk_win11_bypass.enable(false);
                                }

                                let mut state = shared_state.lock().unwrap();
                                state.selected_iso = Some(info);
                            }
                            Err(e) => {
                                let alert = MessageDialog::builder(
                                    &panel_ref,
                                    &format!("Failed to analyze ISO file:\n\n{}", e),
                                    "Invalid ISO File",
                                )
                                .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
                                .build();
                                alert.show_modal();
                            }
                        }
                    }
                }
            }
        });

        // ── Event: START / Burn Button Clicked ───────────────────────────────
        btn_burn.on_click({
            let panel_ref = panel;
            let choice_drives = choice_drives;
            let btn_rescan = btn_rescan;
            let btn_browse_iso = btn_browse_iso;
            let txt_volume_label = txt_volume_label;
            let chk_win11_bypass = chk_win11_bypass;
            let btn_burn = btn_burn;
            let gauge = gauge;
            let lbl_phase = lbl_phase;
            let lbl_stats = lbl_stats;
            let shared_state = Arc::clone(&shared_state);
            let channel_receiver = Arc::clone(&channel_receiver);

            move |_| {
                // 1. Root privilege verification
                if !engine::is_root() {
                    let alert = MessageDialog::builder(
                        &panel_ref,
                        "Root privileges are required to partition and write directly to USB hardware.\n\n\
                         Please launch NextBurn with root privileges:\n\n\
                         sudo nextburn-gui\n\
                         (or run with pkexec nextburn-gui)",
                        "Elevated Privileges Required",
                    )
                    .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
                    .build();
                    alert.show_modal();
                    return;
                }

                // 2. Validate selection
                let (drive, iso) = {
                    let state = shared_state.lock().unwrap();
                    let d = state
                        .selected_drive_idx
                        .and_then(|i| state.drives.get(i).cloned());
                    let iso = state.selected_iso.clone();
                    (d, iso)
                };

                let drive = match drive {
                    Some(d) => d,
                    None => {
                        let alert = MessageDialog::builder(
                            &panel_ref,
                            "Please select a target USB device before starting.",
                            "No Drive Selected",
                        )
                        .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
                        .build();
                        alert.show_modal();
                        return;
                    }
                };

                let iso = match iso {
                    Some(i) => i,
                    None => {
                        let alert = MessageDialog::builder(
                            &panel_ref,
                            "Please select an ISO image file before starting.",
                            "No ISO Selected",
                        )
                        .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
                        .build();
                        alert.show_modal();
                        return;
                    }
                };

                // 3. Destructive Action Confirmation Warning Dialog
                let confirm_msg = format!(
                    "⚠️ WARNING: ALL EXISTING DATA on the target device:\n\n\
                       {} ({:.1} GB — {})\n\n\
                     WILL BE PERMANENTLY ERASED AND OVERWRITTEN!\n\n\
                     Selected ISO Image:\n   {}\n\n\
                     Are you sure you want to proceed and write to this device?",
                    drive.device.display(),
                    drive.size_gb,
                    drive.display_name(),
                    iso.path.display()
                );

                let confirm_dialog = MessageDialog::builder(
                    &panel_ref,
                    &confirm_msg,
                    "Confirm Device Overwrite",
                )
                .with_style(MessageDialogStyle::YesNo | MessageDialogStyle::IconWarning | MessageDialogStyle::NoDefault)
                .build();

                if confirm_dialog.show_modal() != ID_YES {
                    return; // User canceled
                }

                // 4. Disable UI controls during burn
                choice_drives.enable(false);
                btn_rescan.enable(false);
                btn_browse_iso.enable(false);
                txt_volume_label.enable(false);
                chk_win11_bypass.enable(false);
                btn_burn.enable(false);

                gauge.set_value(0);
                lbl_phase.set_label("Starting burn operation...");
                lbl_stats.set_label("Initializing hardware streams...");

                {
                    let mut state = shared_state.lock().unwrap();
                    state.is_burning = true;
                }

                // 5. Spawn background burner thread
                let (tx, rx): (Sender<BurnProgress>, Receiver<BurnProgress>) = mpsc::channel();
                {
                    let mut guard = channel_receiver.lock().unwrap();
                    *guard = Some(rx);
                }

                let iso_path = iso.path.clone();
                let dev_path = drive.device.clone();
                let is_windows = iso.is_windows;
                let volume_label = {
                    let v = txt_volume_label.get_value();
                    if v.trim().is_empty() {
                        "NEXTBURN".to_string()
                    } else {
                        v
                    }
                };
                let bypass = chk_win11_bypass.get_value();

                let tx_thread = tx.clone();
                thread::spawn(move || {
                    let result = if is_windows {
                        engine::burn_windows_iso(&iso_path, &dev_path, &volume_label, bypass, tx_thread.clone())
                    } else {
                        engine::burn_raw_iso(&iso_path, &dev_path, tx_thread.clone())
                    };

                    if let Err(e) = result {
                        tx_thread.send(BurnProgress::failed("Burn Failed", &format!("{}", e))).ok();
                    }
                });
            }
        });

        // ── Event: Timer Tick (Smooth UI Sync) ───────────────────────────────
        timer.on_tick({
            let panel_ref = panel;
            let choice_drives = choice_drives;
            let btn_rescan = btn_rescan;
            let btn_browse_iso = btn_browse_iso;
            let txt_volume_label = txt_volume_label;
            let chk_win11_bypass = chk_win11_bypass;
            let btn_burn = btn_burn;
            let gauge = gauge;
            let lbl_phase = lbl_phase;
            let lbl_stats = lbl_stats;
            let shared_state = Arc::clone(&shared_state);
            let channel_receiver = Arc::clone(&channel_receiver);

            move |_| {
                let mut latest_progress: Option<BurnProgress> = None;
                {
                    let guard = channel_receiver.lock().unwrap();
                    if let Some(ref rx) = *guard {
                        while let Ok(msg) = rx.try_recv() {
                            latest_progress = Some(msg);
                        }
                    }
                }

                if let Some(prog) = latest_progress {
                    let gauge_val = (prog.fraction * 1000.0).clamp(0.0, 1000.0) as i32;
                    gauge.set_value(gauge_val);
                    lbl_phase.set_label(&format!("Status: {}", prog.phase));

                    if prog.total_bytes > 0 {
                        let written_gb = prog.bytes_written as f64 / 1_000_000_000.0;
                        let total_gb = prog.total_bytes as f64 / 1_000_000_000.0;
                        let eta_min = prog.eta_secs / 60;
                        let eta_sec = prog.eta_secs % 60;
                        let eta_str = format!("{:02}:{:02}", eta_min, eta_sec);

                        lbl_stats.set_label(&format!(
                            "Speed: {:.1} MB/s  •  Transferred: {:.2} GB / {:.2} GB ({:.1}%)  •  ETA: {}",
                            prog.speed_mb_s,
                            written_gb,
                            total_gb,
                            prog.fraction * 100.0,
                            eta_str
                        ));
                    }

                    if prog.is_done {
                        {
                            let mut state = shared_state.lock().unwrap();
                            state.is_burning = false;
                        }
                        {
                            let mut guard = channel_receiver.lock().unwrap();
                            *guard = None;
                        }

                        // Re-enable UI
                        choice_drives.enable(true);
                        btn_rescan.enable(true);
                        btn_browse_iso.enable(true);
                        txt_volume_label.enable(true);
                        chk_win11_bypass.enable(true);
                        btn_burn.enable(true);

                        if let Some(err) = prog.error {
                            let alert = MessageDialog::builder(
                                &panel_ref,
                                &format!("Burning encountered an error:\n\n{}", err),
                                "Burn Failed",
                            )
                            .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconError)
                            .build();
                            alert.show_modal();
                            lbl_phase.set_label("Status: Failed.");
                        } else {
                            gauge.set_value(1000);
                            let success_dlg = MessageDialog::builder(
                                &panel_ref,
                                "✅ USB Drive created successfully!\n\n\
                                 The target drive has been flushed and unmounted safely.\n\
                                 You may now safely unplug the USB flash drive.",
                                "Success — NextBurn",
                            )
                            .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconInformation)
                            .build();
                            success_dlg.show_modal();
                            lbl_phase.set_label("Status: Burn complete! Ready for next task.");
                        }
                    }
                }
            }
        });

        // ── Event: Close Button Clicked ──────────────────────────────────────
        btn_close.on_click({
            let frame = frame;
            let shared_state = Arc::clone(&shared_state);
            let panel_ref = panel;

            move |_| {
                let is_burning = shared_state.lock().unwrap().is_burning;
                if is_burning {
                    let alert = MessageDialog::builder(
                        &panel_ref,
                        "A burn operation is currently in progress!\n\nClosing now may corrupt the target USB drive.",
                        "Burn In Progress",
                    )
                    .with_style(MessageDialogStyle::OK | MessageDialogStyle::IconWarning)
                    .build();
                    alert.show_modal();
                } else {
                    frame.close(true);
                }
            }
        });

        frame.centre();
        frame.show(true);
    });
}

// Dedicated binary entry point for `nextburn-gui`
#[allow(dead_code)]
fn main() {
    run_gui();
}
