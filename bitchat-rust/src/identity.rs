//! Manages user identity, including keypairs for encryption and signing.

use ed25519_dalek::{SigningKey, KEYPAIR_LENGTH};
use serde::{Serialize, Deserialize};

/// A serializable version of the `snow::Keypair` struct.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SerializableKeypair {
    pub private: Vec<u8>,
    pub public: Vec<u8>,
}

impl From<snow::Keypair> for SerializableKeypair {
    fn from(keys: snow::Keypair) -> Self {
        Self { private: keys.private, public: keys.public }
    }
}

/// A serializable version of the `ed25519_dalek::SigningKey` struct.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SerializableSigningKeypair {
    #[serde(with = "serde_bytes")]
    pub keypair_bytes: [u8; KEYPAIR_LENGTH],
}

impl From<&SigningKey> for SerializableSigningKeypair {
    fn from(keys: &SigningKey) -> Self {
        Self { keypair_bytes: keys.to_keypair_bytes() }
    }
}

/// Represents a user's identity, containing the necessary cryptographic keys.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UserIdentity {
    /// Keypair for the Noise Protocol (XX pattern).
    /// This is used for establishing encrypted sessions.
    pub noise_keypair: SerializableKeypair,
    /// Keypair for signing messages and announcements.
    pub signing_keypair: SerializableSigningKeypair,
}

impl UserIdentity {
    /// Generates a new identity with fresh keypairs.
    pub fn generate() -> Result<Self, snow::Error> {
        let builder = snow::Builder::new("Noise_XX_25519_ChaChaPoly_SHA256".parse()?);
        let noise_keypair = builder.generate_keypair()?.into();

        let mut csprng = rand::rngs::OsRng{};
        let signing_key: SigningKey = SigningKey::generate(&mut csprng);

        Ok(Self {
            noise_keypair,
            signing_keypair: (&signing_key).into(),
        })
    }

    /// Saves the identity to a file in binary format.
    pub fn save_to_file(&self, path: &str) -> Result<(), std::io::Error> {
        let encoded = bincode::serialize(self).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, encoded)
    }

    /// Loads an identity from a file.
    pub fn load_from_file(path: &str) -> Result<Self, std::io::Error> {
        let data = std::fs::read(path)?;
        let decoded = bincode::deserialize(&data).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        Ok(decoded)
    }
}
