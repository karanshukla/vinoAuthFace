use thiserror::Error;

#[derive(Error, Debug)]
pub enum FaceAuthError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Image error: {0}")]
    Image(#[from] image::ImageError),

    #[error("Inference error: {0}")]
    Inference(String),

    #[error("Verification failed: {0}")]
    VerificationFailed(String),

    #[error("No IR camera found")]
    NoCamera,

    #[error("Camera busy")]
    CameraBusy,

    #[error("No embeddings found for user")]
    NoEmbeddings,

    #[error("Invalid embedding format")]
    InvalidEmbeddingFormat,

    #[error(
        "templates are sealed to the TPM and could not be unsealed (no access to the TPM, or it \
         was cleared or replaced?); if it was replaced, re-enroll with `sudo vinoauthface \
         enroll --user <name>`"
    )]
    SealUnavailable,

    #[error(
        "templates are not TPM-sealed but seal_embeddings is on; re-enroll (or run \
         `vinoauthface improve`) as root to seal them"
    )]
    SealRequired,

    #[error("No face detected in frame")]
    NoFaceDetected,
}

pub type Result<T> = std::result::Result<T, FaceAuthError>;