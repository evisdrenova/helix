use anyhow::Result;
use helix::fsmonitor::FSMonitor;
use std::path::Path;
use std::thread;
use std::time::Duration;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let repo_path = if args.len() > 1 {
        Path::new(&args[1])
    } else {
        Path::new(".")
    };

    println!("Starting FSMonitor for: {:?}", repo_path);
    println!("Make some changes to files in the repo...\n");

    let mut monitor = FSMonitor::new(repo_path)?;
    monitor.start_watching_repo()?;

    // Monitor for 30 seconds
    for i in 0..30 {
        thread::sleep(Duration::from_secs(1));

        let dirty_count = monitor.dirty_count();
        if dirty_count > 0 {
            println!("\n[{}s] Detected {} dirty files:", i + 1, dirty_count);
            for path in monitor.get_dirty_files() {
                println!("  - {:?}", path);
            }
        } else {
            print!(".");
            use std::io::Write;
            std::io::stdout().flush().ok();
        }
    }

    println!("\n\nFinal dirty files:");
    for path in monitor.get_dirty_files() {
        println!("  {:?}", path);
    }

    Ok(())
}
