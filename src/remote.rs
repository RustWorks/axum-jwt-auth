use async_trait::async_trait;
use std::sync::RwLock;

use jsonwebtoken::{
    jwk::{Jwk, JwkSet},
    DecodingKey, TokenData, Validation,
};
use serde::de::DeserializeOwned;

use crate::{Error, JwtDecoder};

/// Remote JWKS decoder.
/// It fetches the JWKS from the given URL and caches it for the given duration.
/// It uses the cached JWKS to decode the JWT tokens.
pub struct RemoteJwksDecoder {
    jwks_url: String,
    jwks_cache_duration: std::time::Duration,
    jwks_cache: RwLock<Vec<Jwk>>,
    validation: Validation,
    client: reqwest::Client,
    retry_count: usize,
    backoff: std::time::Duration,
}

impl RemoteJwksDecoder {
    pub fn new(jwks_url: String) -> Self {
        Self {
            jwks_url,
            jwks_cache_duration: std::time::Duration::from_secs(60 * 60),
            jwks_cache: RwLock::new(Vec::new()),
            validation: Validation::default(),
            client: reqwest::Client::new(),
            retry_count: 3,
            backoff: std::time::Duration::from_secs(1),
        }
    }

    async fn refresh_keys(&self) -> Result<(), Error> {
        let max_attempts = self.retry_count;
        let mut attempt = 0;
        let mut err = None;

        while attempt < max_attempts {
            match self.refresh_keys_once().await {
                Ok(_) => return Ok(()),
                Err(e) => {
                    err = Some(e);
                    attempt += 1;
                    tokio::time::sleep(self.backoff).await;
                }
            }
        }

        // Last attempt failed, return the error
        Err(err.unwrap())
    }

    async fn refresh_keys_once(&self) -> Result<(), Error> {
        let jwks = self
            .client
            .get(&self.jwks_url)
            .send()
            .await?
            .json::<JwkSet>()
            .await?;

        let mut jwks_cache = self.jwks_cache.write().unwrap();
        *jwks_cache = jwks.keys;

        Ok(())
    }

    /// Refreshes the JWKS cache periodically.
    /// It runs in a loop and never returns, so it should be run in a separate tokio task
    /// using [`tokio::spawn`]. If the JWKS refresh fails after multiple attemps,
    /// it logs the error and continues. The decoder will use the stale keys until the next refresh
    /// succeeds or the universe ends, whichever comes first.
    pub async fn refresh_keys_periodically(&self) {
        loop {
            match self.refresh_keys().await {
                Ok(_) => {}
                Err(err) => {
                    // log the error and continue with stale keys
                    eprintln!(
                        "Failed to refresh JWKS after {} attempts: {:?}",
                        self.retry_count, err
                    );
                }
            }
            tokio::time::sleep(self.jwks_cache_duration).await;
        }
    }
}

#[async_trait]
impl<T> JwtDecoder<T> for RemoteJwksDecoder
where
    T: for<'de> DeserializeOwned,
{
    fn decode(&self, token: &str) -> Result<TokenData<T>, Error> {
        let header = jsonwebtoken::decode_header(token)?;
        let kid = header.kid;

        let jwks_cache = self.jwks_cache.read().unwrap();

        // Try to find the key in the cache by kid
        let jwk = jwks_cache.iter().find(|jwk| jwk.common.key_id == kid);
        if let Some(jwk) = jwk {
            let key = DecodingKey::from_jwk(jwk)?;
            return Ok(jsonwebtoken::decode::<T>(token, &key, &self.validation)?);
        }

        // Otherwise, try all the keys in the cache, returning the first one that works
        // If none of them work, return the error from the last one
        let mut err: Option<Error> = None;
        for jwk in jwks_cache.iter() {
            let key = DecodingKey::from_jwk(jwk)?;
            match jsonwebtoken::decode::<T>(token, &key, &self.validation) {
                Ok(token_data) => return Ok(token_data),
                Err(e) => err = Some(e.into()),
            }
        }

        Err(err.unwrap())
    }
}

pub struct RemoteJwksDecoderBuilder {
    jwks_url: String,
    jwks_cache_duration: std::time::Duration,
    validation: Validation,
    client: reqwest::Client,
    retry_count: usize,
    backoff: std::time::Duration,
}

impl RemoteJwksDecoderBuilder {
    pub fn new(jwks_url: String) -> Self {
        Self {
            jwks_url,
            jwks_cache_duration: std::time::Duration::from_secs(60 * 60),
            validation: Validation::default(),
            client: reqwest::Client::new(),
            retry_count: 3,
            backoff: std::time::Duration::from_secs(1),
        }
    }

    pub fn with_jwks_cache_duration(mut self, jwks_cache_duration: std::time::Duration) -> Self {
        self.jwks_cache_duration = jwks_cache_duration;
        self
    }

    pub fn with_client(mut self, client: reqwest::Client) -> Self {
        self.client = client;
        self
    }

    pub fn with_validation(mut self, validation: Validation) -> Self {
        self.validation = validation;
        self
    }

    pub fn with_retry_count(mut self, retry_count: usize) -> Self {
        self.retry_count = retry_count;
        self
    }

    pub fn with_backoff(mut self, backoff: std::time::Duration) -> Self {
        self.backoff = backoff;
        self
    }

    pub fn build(self) -> RemoteJwksDecoder {
        RemoteJwksDecoder {
            jwks_url: self.jwks_url,
            jwks_cache_duration: self.jwks_cache_duration,
            jwks_cache: RwLock::new(Vec::new()),
            validation: Validation::default(),
            client: self.client,
            retry_count: self.retry_count,
            backoff: self.backoff,
        }
    }
}
