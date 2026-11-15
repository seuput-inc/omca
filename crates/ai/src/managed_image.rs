//! Image generation through the app's connected provider runtime.
use crate::provider_runtime::{ImageCapability, ProviderClient};
use base64::Engine as _;
use craft_anything_engines::assets::{AssetProvider, AssetRequest, Generated, ImageModel, Shape, SizeBy};
use craft_anything_evidence::ToolError;
use std::io::Cursor;
use std::sync::Arc;

const MAX_BYTES: usize = 12 * 1024 * 1024;

pub(crate) struct ManagedImage {
    pub client: ProviderClient,
    pub capability: ImageCapability,
    id: &'static str,
}

impl ManagedImage {
    fn make(&self, req: &AssetRequest, cancelled: Arc<dyn Fn() -> bool + Send + Sync>) -> Result<Generated, ToolError> {
        let AssetRequest::Image { prompt, shape, .. } = req else {
            return Err(ToolError::invalid("This provider only generates pictures."));
        };
        let shape = match shape {
            Shape::Square => "square",
            Shape::Wide => "wide",
            Shape::Tall => "tall",
        };
        let result = self.client.generate_image(&self.capability.provider_id, prompt, shape, cancelled)?;
        let (bytes, url) = normalize_image(&result.data_base64, &result.mime_type)?;
        Ok(Generated { url, bytes, cost_usd: result.cost_usd.or(result.estimate_usd).unwrap_or(0.0) })
    }
}

fn normalize_image(data: &str, mime_type: &str) -> Result<(Vec<u8>, String), ToolError> {
    if data.len() > MAX_BYTES * 4 / 3 + 4 {
        return Err(ToolError::invalid("Generated image is too large."));
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(data).map_err(|_| ToolError::invalid("Generated image is not valid base64."))?;
    if bytes.len() > MAX_BYTES {
        return Err(ToolError::invalid("Generated image is too large."));
    }
    let format = image::guess_format(&bytes).map_err(|_| ToolError::invalid("Generated image has an unknown format."))?;
    let expected = match format {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::WebP => "image/webp",
        image::ImageFormat::Jpeg => "image/jpeg",
        _ => return Err(ToolError::invalid("Generated image format is not supported.")),
    };
    if mime_type != expected {
        return Err(ToolError::invalid("Generated image format does not match its MIME type."));
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(&bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|_| ToolError::invalid("Generated image could not be decoded."))?;
    let png = if format == image::ImageFormat::Png {
        bytes
    } else {
        let mut out = Cursor::new(Vec::new());
        decoded.write_to(&mut out, image::ImageFormat::Png).map_err(|_| ToolError::invalid("Generated image could not be converted to PNG."))?;
        out.into_inner()
    };
    if png.len() > MAX_BYTES {
        return Err(ToolError::invalid("Generated image is too large."));
    }
    let url = if format == image::ImageFormat::Png {
        format!("data:image/png;base64,{data}")
    } else {
        format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(&png))
    };
    Ok((png, url))
}

impl AssetProvider for ManagedImage {
    fn id(&self) -> &'static str {
        self.id
    }
    fn name(&self) -> &'static str {
        "Connected GPT image generation"
    }
    fn requires_key(&self) -> bool {
        false
    }
    fn cost_is_unknown(&self) -> bool {
        self.capability.estimate_usd.is_none()
    }
    fn preferred_image_model(&self) -> Option<ImageModel> {
        Some(ImageModel {
            id: self.capability.model_id.clone(),
            provider: self.id().into(),
            name: self.capability.name.clone(),
            endpoint: self.capability.model_id.clone(),
            usd_per_megapixel: 0.0,
            usd_per_image: self.capability.estimate_usd.unwrap_or(0.0),
            size_by: SizeBy::default(),
            licence: String::new(),
        })
    }
    fn generate(&self, _key: &str, req: &AssetRequest) -> Result<Generated, ToolError> {
        self.make(req, Arc::new(|| false))
    }
    fn generate_cancelled(&self, _key: &str, req: &AssetRequest, cancelled: Arc<dyn Fn() -> bool + Send + Sync>) -> Result<Generated, ToolError> {
        self.make(req, cancelled)
    }
}

pub(crate) struct ManagedImageContext {
    pub client: ProviderClient,
    pub preferred: Option<String>,
    pub stop: craft_anything_platform::cancel::CancelToken,
}

/// Install connected image routes into an isolated tool session without exposing credentials.
pub fn install_managed_image_providers(
    session: &mut craft_anything_engine::Session,
    client: ProviderClient,
    preferred: Option<String>,
    stop: craft_anything_platform::cancel::CancelToken,
) -> bool {
    let context = Arc::new(ManagedImageContext { client, preferred, stop });
    let available = refresh(session, &context);
    session.ext.insert(context);
    available
}

pub(crate) fn refresh(session: &mut craft_anything_engine::Session, context: &ManagedImageContext) -> bool {
    use craft_anything_engines::assets::AssetProviders;
    let mut providers = session.ext.get::<AssetProviders>().map(|p| p.as_ref().clone()).unwrap_or_default();
    providers.remove("managed-openai");
    providers.remove("managed-openai-codex");
    let stop = context.stop.clone();
    let selected = match context.client.image_capabilities_cancelled(Arc::new(move || stop.is_cancelled())) {
        Ok(mut caps) => {
            providers.set_image_discovery_error(None);
            caps.retain(|c| matches!(c.provider_id.as_str(), "openai" | "openai-codex"));
            caps.sort_by(|a, b| {
                let rank = |id: &str| if Some(id) == context.preferred.as_deref() { 0 } else { 1 };
                rank(&a.provider_id).cmp(&rank(&b.provider_id)).then_with(|| a.provider_id.cmp(&b.provider_id)).then_with(|| a.model_id.cmp(&b.model_id))
            });
            caps.into_iter().next()
        }
        Err(error) => {
            providers.set_image_discovery_error(Some(error));
            None
        }
    };
    let selected = selected.and_then(|capability| {
        let id = match capability.provider_id.as_str() {
            "openai" => "managed-openai",
            "openai-codex" => "managed-openai-codex",
            _ => return None,
        };
        Some(ManagedImage { client: context.client.clone(), capability, id })
    });
    let available = selected.is_some();
    if let Some(provider) = selected {
        providers.register(Arc::new(provider));
    }
    session.ext.insert(Arc::new(providers));
    available
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_images_become_decodable_png_data_uris() -> Result<(), Box<dyn std::error::Error>> {
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(2, 3, image::Rgb([40, 80, 120])));
        for (format, mime) in [(image::ImageFormat::Png, "image/png"), (image::ImageFormat::WebP, "image/webp"), (image::ImageFormat::Jpeg, "image/jpeg")] {
            let mut input = Cursor::new(Vec::new());
            image.write_to(&mut input, format)?;
            let encoded = base64::engine::general_purpose::STANDARD.encode(input.into_inner());
            let (png, uri) = normalize_image(&encoded, mime)?;
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
            let decoded = image::load_from_memory_with_format(&png, image::ImageFormat::Png)?;
            assert_eq!((decoded.width(), decoded.height()), (2, 3));
            let inline = uri.strip_prefix("data:image/png;base64,").ok_or("Missing PNG data URI")?;
            assert_eq!(base64::engine::general_purpose::STANDARD.decode(inline)?, png);
            assert!(normalize_image(&encoded, "image/gif").is_err());
        }
        Ok(())
    }

    #[test]
    fn oversized_image_data_is_rejected_before_decoding() {
        let oversized = "A".repeat(MAX_BYTES * 4 / 3 + 5);
        assert!(normalize_image(&oversized, "image/png").is_err());
    }
}
