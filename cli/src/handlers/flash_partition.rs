use std::path::PathBuf;

use lfff_lib::flasher::{collect_images, run_flash_single};

pub fn run(
    image: Option<&str>,
    firmware_dir: Option<&PathBuf>,
    partition: Option<&str>,
    slot: Option<&str>,
    no_ab: bool,
    dry_run: bool,
    serial: Option<&str>,
) -> i32 {
    let image_path: PathBuf = match image {
        Some(img) => {
            let p = PathBuf::from(img);
            if p.extension().map(|e| e == "img").unwrap_or(false) && p.exists() {
                p
            } else {
                let part_name = p
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_lowercase();
                match firmware_dir {
                    Some(dir) => {
                        let images = collect_images(dir);
                        match images.get(&part_name) {
                            Some(path) => path.clone(),
                            None => {
                                let available: Vec<_> = images.keys().collect();
                                println!(
                                    "✗ Partition '{}' not found in {}",
                                    part_name,
                                    dir.display()
                                );
                                println!("  Available: {:?}", available);
                                return 1;
                            }
                        }
                    }
                    None => {
                        println!(
                            "✗ '{}' is not a .img file and --firmware-dir is not set.",
                            img
                        );
                        return 1;
                    }
                }
            }
        }
        None => {
            println!("✗ Provide an image path or partition name with --firmware-dir.");
            return 1;
        }
    };

    let slots: Option<Vec<String>> = if let Some(s) = slot {
        Some(s.split(',').map(|x| x.trim().to_lowercase()).collect())
    } else if no_ab {
        Some(vec![String::new()])
    } else {
        None
    };

    let session = run_flash_single(&image_path, partition, slots.as_deref(), serial, dry_run);

    if session.critical_failed().is_empty() {
        0
    } else {
        1
    }
}
