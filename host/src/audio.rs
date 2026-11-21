//! Audio output for the windowed frontend.
//!
//! The guest only ever names a clip (for example `B7B`); the host maps that name to a
//! file in the assets directory and decodes it with pure-Rust decoders. Clip names are
//! restricted to letters and digits, so a name can never point outside the assets.

// Only the windowed frontend plays sound; a headless build just describes the requests.
#![cfg_attr(not(feature = "window"), allow(dead_code, unused_imports))]

use crate::java::AudioCmd;
use std::path::{Path, PathBuf};

fn clip_path(assets: &Path, kind: &str, name: &str, ext: &str) -> Option<PathBuf> {
    if name.is_empty() || name.len() > 32 || !name.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let p = assets.join("Default").join(kind).join(format!("{name}.{ext}"));
    p.is_file().then_some(p)
}

#[cfg(feature = "audio")]
mod imp {
    use super::*;
    use rodio::queue::SourcesQueueOutput;
    use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::BufReader;

    /// Where finished audio goes.
    enum Output {
        /// The system's default sound device.
        Device {
            /// Dropping the stream closes the device, so it is held for as long as `handle`.
            _stream: OutputStream,
            handle: OutputStreamHandle,
        },
        /// Kept in memory so a test can read the samples a device would have received.
        #[cfg_attr(not(test), allow(dead_code))]
        Capture(Vec<SourcesQueueOutput<f32>>),
        None,
    }

    pub struct Audio {
        out: Output,
        assets: PathBuf,
        music: HashMap<String, Sink>,
        sfx: Vec<Sink>,
        sfx_volume: f32,
        music_volume: f32,
    }

    impl Audio {
        pub fn new(assets: &Path) -> Audio {
            let out = match OutputStream::try_default() {
                Ok((stream, handle)) => Output::Device { _stream: stream, handle },
                Err(e) => {
                    eprintln!("[host] no audio output ({e}); continuing without sound");
                    Output::None
                }
            };
            Self::with_output(assets, out)
        }

        /// An `Audio` with no device: everything it would play is kept for [`Audio::captured`].
        #[cfg(test)]
        pub fn capture(assets: &Path) -> Audio {
            Self::with_output(assets, Output::Capture(Vec::new()))
        }

        /// The streams produced so far, one per clip started, in order.
        #[cfg(test)]
        pub fn captured(&mut self) -> &mut Vec<SourcesQueueOutput<f32>> {
            match &mut self.out {
                Output::Capture(v) => v,
                _ => panic!("not a capturing Audio"),
            }
        }

        fn with_output(assets: &Path, out: Output) -> Audio {
            Audio { out, assets: assets.to_path_buf(), music: HashMap::new(), sfx: Vec::new(), sfx_volume: 1.0, music_volume: 1.0 }
        }

        /// Decode a clip and make a sink for it whose output is already routed to `out`.
        fn open(&mut self, kind: &str, name: &str, ext: &str) -> Option<(Sink, Decoder<BufReader<File>>)> {
            if matches!(self.out, Output::None) {
                return None;
            }
            let file = File::open(clip_path(&self.assets, kind, name, ext)?).ok()?;
            let source = Decoder::new(BufReader::new(file)).ok()?;
            let (sink, stream) = Sink::new_idle();
            match &mut self.out {
                Output::Device { handle, .. } => handle.play_raw(stream).ok()?,
                Output::Capture(streams) => streams.push(stream),
                Output::None => return None,
            }
            Some((sink, source))
        }

        pub fn apply(&mut self, cmd: AudioCmd) {
            match cmd {
                AudioCmd::PlaySfx(name) => {
                    self.sfx.retain(|s| !s.empty());
                    if self.sfx.len() >= 16 {
                        return;
                    }
                    if let Some((sink, source)) = self.open("W", &name, "ogg") {
                        sink.set_volume(self.sfx_volume);
                        sink.append(source);
                        self.sfx.push(sink);
                    }
                }
                AudioCmd::PlayMusic { name, looped } => {
                    if let Some(old) = self.music.remove(&name) {
                        old.stop();
                    }
                    if let Some((sink, source)) = self.open("B", &name, "mp3") {
                        sink.set_volume(self.music_volume);
                        if looped {
                            sink.append(source.repeat_infinite());
                        } else {
                            sink.append(source);
                        }
                        self.music.insert(name, sink);
                    }
                }
                AudioCmd::StopMusic(name) => {
                    if let Some(sink) = self.music.remove(&name) {
                        sink.stop();
                    }
                }
                AudioCmd::SfxVolume(v) => self.sfx_volume = v,
                AudioCmd::MusicVolume(v) => {
                    self.music_volume = v;
                    for s in self.music.values() {
                        s.set_volume(v);
                    }
                }
            }
        }
    }
}

#[cfg(not(feature = "audio"))]
mod imp {
    use super::*;

    pub struct Audio;

    impl Audio {
        pub fn new(_assets: &Path) -> Audio {
            eprintln!("[host] built without the `audio` feature; continuing without sound");
            Audio
        }
        pub fn apply(&mut self, _cmd: AudioCmd) {}
    }
}

pub use imp::Audio;

/// Whether the clip a command names exists, for logging in headless runs.
pub fn describe(assets: &Path, cmd: &AudioCmd) -> String {
    let found = |kind, name: &str, ext| if clip_path(assets, kind, name, ext).is_some() { "" } else { " (no such clip)" };
    match cmd {
        AudioCmd::PlaySfx(n) => format!("play sound {n}{}", found("W", n, "ogg")),
        AudioCmd::PlayMusic { name, looped } => format!("play music {name}{}{}", if *looped { " (looping)" } else { "" }, found("B", name, "mp3")),
        AudioCmd::StopMusic(n) => format!("stop music {n}"),
        AudioCmd::SfxVolume(v) => format!("sound volume {:.0}%", v * 100.0),
        AudioCmd::MusicVolume(v) => format!("music volume {:.0}%", v * 100.0),
    }
}
