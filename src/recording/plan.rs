use std::path::PathBuf;

use anyhow::{bail, Result};

use crate::{cli::CaptureArgs, config::RecordingConfig};

use super::SAMPLE_RATE;

pub(super) struct RecordingPlan {
    pub(super) output: Option<PathBuf>,
    pub(super) force: bool,
    pub(super) duration_seconds: Option<u64>,
    pub(super) microphone: Option<SourcePlan>,
    pub(super) system: Option<SourcePlan>,
}

pub(super) struct SourcePlan {
    pub(super) device_name: Option<String>,
}

impl RecordingPlan {
    pub(super) fn from_args(args: CaptureArgs, config: Option<&RecordingConfig>) -> Result<Self> {
        if args.no_microphone && args.no_system_audio {
            bail!("at least one audio source must be enabled")
        }

        let microphone = (!args.no_microphone).then(|| SourcePlan {
            device_name: config.and_then(|config| config.microphone_device.clone()),
        });
        let system = (!args.no_system_audio).then(|| SourcePlan {
            device_name: config.and_then(|config| config.system_device.clone()),
        });

        Ok(Self {
            output: args.output,
            force: args.force,
            duration_seconds: args.duration,
            microphone,
            system,
        })
    }

    pub(super) fn sample_limit(&self) -> Option<usize> {
        self.duration_seconds
            .map(|seconds| seconds.saturating_mul(SAMPLE_RATE as u64) as usize)
    }

    pub(super) fn microphone_enabled(&self) -> bool {
        self.microphone.is_some()
    }

    pub(super) fn system_enabled(&self) -> bool {
        self.system.is_some()
    }
}
