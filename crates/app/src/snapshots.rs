//! Pictures of the chart as trades close: drawn offscreen and written as PNG files on a thread of their own, so
//! the window does not wait for them.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use tickerlight_backend_wgpu::{Gpu, render_to_rgba};
use tickerlight_core::Scene;

struct Job {
    scene: Scene,
    size: (u32, u32),
    path: PathBuf,
}

/// The snapshots asked for and the thread that writes them, started with the first. Dropped, it writes those
/// still waiting before it goes.
#[derive(Default)]
pub struct Snapshots {
    worker: Option<(Sender<Job>, JoinHandle<()>, Receiver<String>)>,
}

impl Snapshots {
    /// Writes `scene`, `size` device pixels, to `path` (its folder made if need be).
    pub fn take(&mut self, scene: Scene, size: (u32, u32), path: PathBuf) {
        let (jobs, _, _) = self.worker.get_or_insert_with(spawn);
        // The thread ends only when this side is dropped.
        let _ = jobs.send(Job { scene, size, path });
    }

    /// Why a snapshot failed since the last call, the latest of them.
    pub fn failure(&self) -> Option<String> {
        self.worker.as_ref().and_then(|(_, _, failed)| failed.try_iter().last())
    }
}

impl Drop for Snapshots {
    fn drop(&mut self) {
        if let Some((jobs, thread, _)) = self.worker.take() {
            drop(jobs);
            let _ = thread.join();
        }
    }
}

fn spawn() -> (Sender<Job>, JoinHandle<()>, Receiver<String>) {
    let (jobs, waiting) = channel::<Job>();
    let (fail, failed) = channel();
    let thread = std::thread::spawn(move || {
        // The device is opened for the first snapshot and kept for the next.
        let mut gpu = None;
        for job in waiting {
            if let Err(e) = write(&mut gpu, &job) {
                let _ = fail.send(format!("Snapshot not saved: {e}"));
            }
        }
    });
    (jobs, thread, failed)
}

fn write(gpu: &mut Option<Gpu>, job: &Job) -> Result<(), String> {
    let gpu = match gpu {
        Some(gpu) => gpu,
        None => gpu.insert(Gpu::new_headless()?),
    };
    let (w, h) = job.size;
    let rgba = render_to_rgba(gpu, &job.scene, w, h);
    if let Some(dir) = job.path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let file = std::fs::File::create(&job.path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&rgba).map_err(|e| e.to_string())
}
