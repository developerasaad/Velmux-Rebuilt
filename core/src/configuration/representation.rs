use super::OutputFormat;
use crate::EngineError;
use serde::{Deserialize, Serialize};

/// The actual representation frozen at acceptance, separate from format intent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputRepresentation {
    pub format: OutputFormat,
    pub muxer: String,
    pub extension: String,
    pub mime_type: String,
}

impl OutputRepresentation {
    pub(crate) fn from_format(format: OutputFormat) -> Self {
        Self {
            format,
            muxer: format.muxer().into(),
            extension: format.extension(),
            mime_type: format.mime_type(),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), EngineError> {
        let safe_extension = !self.extension.is_empty()
            && self.extension.len() <= 16
            && self
                .extension
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
        if !safe_extension || self != &Self::from_format(self.format) {
            return Err(EngineError::Recovery {
                detail: "The accepted output representation is invalid or unsupported. Its working resources have been preserved.".into(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_representations_match_the_canonical_media_catalog() {
        for format in [
            OutputFormat::Mp4,
            OutputFormat::Matroska,
            OutputFormat::M4a,
            OutputFormat::Wav,
            OutputFormat::Flac,
            OutputFormat::Mp3,
        ] {
            let output = OutputRepresentation::from_format(format);
            output.validate().unwrap();
            assert_eq!(output.extension, format.extension());
            assert_eq!(output.mime_type, format.mime_type());
        }
    }

    #[test]
    fn persisted_output_descriptors_cannot_select_arbitrary_paths_or_muxers() {
        for extension in ["../mp4", "mp4/escape", ".mp4", "mp4\0", "", "MP4"] {
            let mut output = OutputRepresentation::from_format(OutputFormat::Mp4);
            output.extension = extension.into();
            assert!(matches!(
                output.validate(),
                Err(EngineError::Recovery { .. })
            ));
        }
        let mut output = OutputRepresentation::from_format(OutputFormat::Mp4);
        output.muxer = "matroska".into();
        assert!(matches!(
            output.validate(),
            Err(EngineError::Recovery { .. })
        ));
        let mut output = OutputRepresentation::from_format(OutputFormat::Mp4);
        output.mime_type = "audio/mp4".into();
        assert!(matches!(
            output.validate(),
            Err(EngineError::Recovery { .. })
        ));
    }
}
