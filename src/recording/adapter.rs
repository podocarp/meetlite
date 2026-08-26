use anyhow::Result;

use super::{microphone::MicrophoneCapture, CaptureStatistics, SourceBuffer};

pub(super) trait CaptureAdapter {
    fn sample_rate(&self) -> u32;
    fn drain_into(&mut self, output: &mut SourceBuffer);
    fn stop(self: Box<Self>, output: &mut SourceBuffer) -> CaptureStatistics;
}

pub(super) type BoxedCaptureAdapter = Box<dyn CaptureAdapter>;

impl CaptureAdapter for MicrophoneCapture {
    fn sample_rate(&self) -> u32 {
        MicrophoneCapture::sample_rate(self)
    }

    fn drain_into(&mut self, output: &mut SourceBuffer) {
        MicrophoneCapture::drain_into(self, output);
    }

    fn stop(self: Box<Self>, output: &mut SourceBuffer) -> CaptureStatistics {
        (*self).stop(output)
    }
}

#[cfg(target_os = "macos")]
impl CaptureAdapter for super::macos_capture_agent::AgentSystemAudioCapture {
    fn sample_rate(&self) -> u32 {
        super::macos_capture_agent::AgentSystemAudioCapture::sample_rate(self)
    }

    fn drain_into(&mut self, output: &mut SourceBuffer) {
        super::macos_capture_agent::AgentSystemAudioCapture::drain_into(self, output);
    }

    fn stop(self: Box<Self>, output: &mut SourceBuffer) -> CaptureStatistics {
        (*self).stop(output)
    }
}

#[cfg(target_os = "linux")]
impl CaptureAdapter for super::linux_system::SystemAudioCapture {
    fn sample_rate(&self) -> u32 {
        super::linux_system::SystemAudioCapture::sample_rate(self)
    }

    fn drain_into(&mut self, output: &mut SourceBuffer) {
        super::linux_system::SystemAudioCapture::drain_into(self, output);
    }

    fn stop(self: Box<Self>, output: &mut SourceBuffer) -> CaptureStatistics {
        (*self).stop(output)
    }
}

pub(super) trait CaptureAdapterFactory {
    fn start_microphone(&self, device_name: Option<&str>) -> Result<BoxedCaptureAdapter>;
    fn start_system_audio(&self, device_name: Option<&str>) -> Result<BoxedCaptureAdapter>;
}

pub(super) struct PlatformCaptureAdapterFactory;

impl CaptureAdapterFactory for PlatformCaptureAdapterFactory {
    fn start_microphone(&self, device_name: Option<&str>) -> Result<BoxedCaptureAdapter> {
        MicrophoneCapture::start(device_name)
            .map(|capture| Box::new(capture) as BoxedCaptureAdapter)
    }

    #[cfg(target_os = "macos")]
    fn start_system_audio(&self, device_name: Option<&str>) -> Result<BoxedCaptureAdapter> {
        let _ = device_name;
        super::macos_capture_agent::AgentSystemAudioCapture::start()
            .map(|capture| Box::new(capture) as BoxedCaptureAdapter)
    }

    #[cfg(target_os = "linux")]
    fn start_system_audio(&self, device_name: Option<&str>) -> Result<BoxedCaptureAdapter> {
        super::linux_system::SystemAudioCapture::start(device_name)
            .map(|capture| Box::new(capture) as BoxedCaptureAdapter)
    }
}
