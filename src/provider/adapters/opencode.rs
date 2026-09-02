use super::*;
pub struct OpenCodeAdapter;
impl ProviderAdapter for OpenCodeAdapter {
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
            target.auth_scheme,
        )
    }
}
