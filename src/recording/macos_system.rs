//! macOS global system-audio capture through a small Core Audio bridge.
//!
//! The bridge follows Meetily's process-tap plus private aggregate-device
//! design. Keeping Objective-C at this boundary avoids a large dependency and
//! lets the recorder use ordinary Rust channels after capture ingress.

use std::{
    ffi::{c_char, c_void, CStr},
    ptr::NonNull,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use anyhow::Result;
use crossbeam_channel::{bounded, Receiver, Sender};

use super::{AudioFrame, CaptureStatistics, SourceKind};

const CALLBACK_QUEUE_CAPACITY: usize = 128;
const ERROR_BUFFER_LENGTH: usize = 512;

pub struct SystemAudioCapture {
    receiver: Receiver<AudioFrame>,
    context: Box<CallbackContext>,
    handle: Option<NonNull<NativeCapture>>,
    dropped_frames: Arc<AtomicU64>,
}

struct CallbackContext {
    sender: Sender<AudioFrame>,
    dropped_frames: Arc<AtomicU64>,
}

#[repr(C)]
struct NativeCapture {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn meetlite_system_audio_start(
        callback: unsafe extern "C" fn(*const f32, usize, *mut c_void),
        context: *mut c_void,
        error: *mut c_char,
        error_length: usize,
    ) -> *mut NativeCapture;
    fn meetlite_system_audio_sample_rate(capture: *const NativeCapture) -> f64;
    fn meetlite_system_audio_stop(capture: *mut NativeCapture);
}

impl SystemAudioCapture {
    pub fn start() -> Result<Self> {
        let (sender, receiver) = bounded(CALLBACK_QUEUE_CAPACITY);
        let dropped_frames = Arc::new(AtomicU64::new(0));
        let mut context = Box::new(CallbackContext {
            sender,
            dropped_frames: Arc::clone(&dropped_frames),
        });
        let mut error = [0 as c_char; ERROR_BUFFER_LENGTH];
        let handle = unsafe {
            meetlite_system_audio_start(
                on_audio,
                (&mut *context as *mut CallbackContext).cast(),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        let handle = NonNull::new(handle).ok_or_else(|| native_error(&error))?;

        Ok(Self {
            receiver,
            context,
            handle: Some(handle),
            dropped_frames,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        unsafe {
            meetlite_system_audio_sample_rate(
                self.handle.expect("capture handle is present").as_ptr(),
            )
        }
        .round() as u32
    }

    pub fn drain_into(&mut self, output: &mut super::SourceBuffer) {
        while let Ok(frame) = self.receiver.try_recv() {
            output.push(frame);
        }
    }

    pub fn stop(mut self, output: &mut super::SourceBuffer) -> CaptureStatistics {
        if let Some(handle) = self.handle.take() {
            unsafe { meetlite_system_audio_stop(handle.as_ptr()) };
        }
        self.drain_into(output);
        CaptureStatistics {
            dropped_callback_frames: self.dropped_frames.load(Ordering::Relaxed),
            dropped_buffered_frames: output.dropped_frames(),
        }
    }
}

impl Drop for SystemAudioCapture {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            unsafe { meetlite_system_audio_stop(handle.as_ptr()) };
        }
        // Keep the callback context alive until native capture has stopped.
        let _ = &self.context;
    }
}

unsafe extern "C" fn on_audio(samples: *const f32, sample_count: usize, context: *mut c_void) {
    if samples.is_null() || context.is_null() || sample_count == 0 {
        return;
    }

    let context = unsafe { &*(context as *const CallbackContext) };
    let samples = unsafe { std::slice::from_raw_parts(samples, sample_count) };
    if context
        .sender
        .try_send(AudioFrame {
            source: SourceKind::System,
            captured_at: Instant::now(),
            sample_rate: 48_000,
            samples: samples.to_vec(),
        })
        .is_err()
    {
        context.dropped_frames.fetch_add(1, Ordering::Relaxed);
    }
}

fn native_error(error: &[c_char]) -> anyhow::Error {
    let message = unsafe { CStr::from_ptr(error.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    if message.is_empty() {
        anyhow::anyhow!("could not start Core Audio system capture")
    } else {
        anyhow::anyhow!(message)
    }
}
