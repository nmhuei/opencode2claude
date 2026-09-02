use super::*;
pub struct KiloAdapter;
impl ProviderAdapter for KiloAdapter {
    fn prepare(
        &self,
        provider: &Provider,
        target: &AttemptTarget,
        request: &ProviderRequest,
        credential: Option<&SecretString>,
    ) -> Result<ProviderHttpRequest, AdapterError> {
        openai_request(
            provider,
            target,
            request,
            credential,
            "/chat/completions",
            if credential.is_some() {
                super::super::types::AuthScheme::Bearer
            } else {
                super::super::types::AuthScheme::None
            },
        )
    }
}
