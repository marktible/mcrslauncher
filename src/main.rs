#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ui;

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "MCRS Launcher {}\n\nUsage: mcrslauncher [--page play|mods|updates|settings]\n\nMCRSLAUNCHER_HOME sets the application data folder.",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    let initial_page = args
        .windows(2)
        .find(|pair| pair[0] == "--page")
        .map(|pair| pair[1].clone())
        .unwrap_or_default();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("MCRS · Horizon Launcher")
            .with_app_id(std::env::var("FLATPAK_ID").unwrap_or_else(|_| "mcrslauncher".into()))
            .with_icon(icon())
            .with_inner_size([1140., 780.])
            .with_min_inner_size([480., 360.])
            .with_resizable(true),
        renderer: eframe::Renderer::Glow,
        vsync: true,
        ..Default::default()
    };
    eframe::run_native(
        "MCRS Launcher",
        options,
        Box::new(move |cc| Ok(Box::new(ui::Launcher::new(cc, &initial_page)))),
    )
}

fn icon() -> eframe::egui::IconData {
    let mut rgba = Vec::with_capacity(64 * 64 * 4);
    for y in 0_i32..64 {
        for x in 0_i32..64 {
            let dx = x - 32;
            let dy = y - 20;
            let color = if dx.abs() + dy.abs() * 2 < 38 {
                [231, 151, 92, 255]
            } else if y > 20 && y < 49 && dx.abs() < 20 && y < 58 - dx.abs() / 2 {
                if dx < 0 {
                    [150, 98, 60, 255]
                } else {
                    [189, 124, 75, 255]
                }
            } else {
                [26, 30, 31, 255]
            };
            rgba.extend_from_slice(&color);
        }
    }
    eframe::egui::IconData {
        rgba,
        width: 64,
        height: 64,
    }
}
