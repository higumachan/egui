use ahash::HashMap;
use egui::{load::{BytesPoll, ImageLoadResult, ImageLoader, ImagePoll, LoadError, SizeHint}, mutex::Mutex, ColorImage, TextBuffer};
use std::{mem::size_of, path::Path, sync::Arc};
use image::DynamicImage;
use image::imageops::{contrast, rotate180, rotate270, rotate90};
use image::imageops::colorops::{brighten_in_place, contrast_in_place};
use url::form_urlencoded::parse;
use url::Url;
use crate::image::load_image_bytes;

type Entry = Result<Arc<ColorImage>, String>;

#[derive(Default)]
pub struct ImageCrateLoader {
    cache: Mutex<HashMap<String, Entry>>,
}

impl ImageCrateLoader {
    pub const ID: &'static str = egui::generate_loader_id!(ImageCrateLoader);
}

/// Parameters for image processing.
///
/// # Examples
///
/// ```rust
/// use egui_extras::ImageProcessingParameter;
///
/// let params = ImageProcessingParameter::new(1, 0.5, 10);
///
/// assert_eq!(params.into_url_params(), "?rotate-90-angle=1&contrast=0.5&brighten=10");
/// ```
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImageProcessingParameter {
    /// Affine transformations Rotate an image 90 degrees clockwise.
    pub rotate_90_angle: u8,
    /// [image::color_ops::contrast](https://docs.rs/image/latest/image/imageops/colorops/fn.contrast.html) parameter
    pub contrast: f32,
    /// [image::color_ops::brighten](https://docs.rs/image/latest/image/imageops/colorops/fn.brighten.html) parameter
    pub brighten: i32,
}

const ROTATE_90_ANGLE: &str = "rotate-90-angle";
const CONTRAST: &str = "contrast";
const BRIGHTEN: &str = "brighten";

impl ImageProcessingParameter {
    pub fn new(rotate_90_angle: u8, contrast: f32, brighten: i32) -> Self {
        Self {
            rotate_90_angle,
            contrast,
            brighten,
        }
    }

    pub fn into_url_params(&self) -> String {
        format!("?{}={}&{}={}&{}={}", ROTATE_90_ANGLE, self.rotate_90_angle, CONTRAST, self.contrast, BRIGHTEN, self.brighten)
    }

    pub fn parse_from_url(url: &str) -> Result<Self, String> {
        let parsed = Url::parse(url).map_err(|e| format!("Failed to parse URL: {}", e))?;

        let mut params = Self::default();
        for (key, value) in parsed.query_pairs() {
            match key.as_str() {
                ROTATE_90_ANGLE => {
                    let rotate_90_angle = value.parse().map_err(|_| format!("Failed to parse rotate_90_angle value: {}", value))?;
                    if rotate_90_angle > 3 {
                        return Err(format!("rotate_90_angle value must be between 0 and 3, got: {}", rotate_90_angle));
                    }
                    params.rotate_90_angle = rotate_90_angle;
                }
                CONTRAST => {
                    let contrast = value.parse().map_err(|_| format!("Failed to parse contrast value: {}", value))?;
                    if contrast < -1.0 || contrast > 1.0 {
                        return Err(format!("contrast value must be between -1.0 and 1.0, got: {}", contrast));
                    }
                    params.contrast = contrast;
                }
                BRIGHTEN => {
                    let brighten = value.parse().map_err(|_| format!("Failed to parse brighten value: {}", value))?;
                    params.brighten = brighten;
                }
                _ => {},
            }
        }

        Ok(params)
    }

    pub fn apply(&self, image: &DynamicImage) -> Result<DynamicImage, String> {
        let mut image = match self.rotate_90_angle {
            1 => rotate90(image),
            2 => rotate180(image),
            3 => rotate270(image),
            _ => image.as_rgba8().ok_or_else(|| "Failed to convert image to RGBA8")?.clone(),
        };

        if self.contrast != 0.0 {
            contrast_in_place(&mut image, self.contrast)
        }
        if self.brighten != 0 {
            brighten_in_place(&mut image, self.brighten)
        }

        Ok(image.into())
    }
}

fn is_supported_uri(uri: &str) -> bool {
    // TODO(emilk): use https://github.com/image-rs/image/pull/2038 when new `image` crate is released.
    let Some(ext) = Path::new(uri).extension().and_then(|ext| ext.to_str()) else {
        // `true` because if there's no extension, assume that we support it
        return true;
    };

    ext != "svg"
}

fn is_unsupported_mime(mime: &str) -> bool {
    // TODO(emilk): use https://github.com/image-rs/image/pull/2038 when new `image` crate is released.
    mime.contains("svg")
}

impl ImageLoader for ImageCrateLoader {
    fn id(&self) -> &str {
        Self::ID
    }

    fn load(&self, ctx: &egui::Context, uri: &str, _: SizeHint) -> ImageLoadResult {
        // three stages of guessing if we support loading the image:
        // 1. URI extension
        // 2. Mime from `BytesPoll::Ready`
        // 3. image::guess_format

        // (1)
        if !is_supported_uri(uri) {
            return Err(LoadError::NotSupported);
        }

        let mut cache = self.cache.lock();
        if let Some(entry) = cache.get(uri).cloned() {
            match entry {
                Ok(image) => Ok(ImagePoll::Ready { image }),
                Err(err) => Err(LoadError::Loading(err)),
            }
        } else {
            match ctx.try_load_bytes(uri) {
                Ok(BytesPoll::Ready { bytes, mime, .. }) => {
                    // (2 and 3)
                    if mime.as_deref().is_some_and(is_unsupported_mime)
                        || image::guess_format(&bytes).is_err()
                    {
                        return Err(LoadError::NotSupported);
                    }

                    log::trace!("started loading {uri:?}");
                    let result = image::load_from_memory(&bytes).map_err(|err| err.to_string());
                    log::trace!("finished loading {uri:?}");
                    log::trace!("started processing {uri:?}");
                    let result = result.and_then(|image| {
                        let params = ImageProcessingParameter::parse_from_url(uri).map_err(|err| err.to_string())?;
                        let image = params.apply(&image)?;

                        let size = [image.width() as _, image.height() as _];
                        let image_buffer = image.to_rgba8();
                        let pixels = image_buffer.as_flat_samples();
                        Ok(Arc::new(ColorImage::from_rgba_unmultiplied(
                            size,
                            pixels.as_slice(),
                        )))
                    });
                    log::trace!("finished processing {uri:?}");
                    cache.insert(uri.into(), result.clone());
                    match result {
                        Ok(image) => Ok(ImagePoll::Ready { image }),
                        Err(err) => Err(LoadError::Loading(err)),
                    }
                }
                Ok(BytesPoll::Pending { size }) => Ok(ImagePoll::Pending { size }),
                Err(err) => Err(err),
            }
        }
    }

    fn forget(&self, uri: &str) {
        let _ = self.cache.lock().remove(uri);
    }

    fn forget_all(&self) {
        self.cache.lock().clear();
    }

    fn byte_size(&self) -> usize {
        self.cache
            .lock()
            .values()
            .map(|result| match result {
                Ok(image) => image.pixels.len() * size_of::<egui::Color32>(),
                Err(err) => err.len(),
            })
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_support() {
        assert!(is_supported_uri("https://test.png"));
        assert!(is_supported_uri("test.jpeg"));
        assert!(is_supported_uri("http://test.gif"));
        assert!(is_supported_uri("test.webp"));
        assert!(is_supported_uri("file://test"));
        assert!(!is_supported_uri("test.svg"));
    }

    #[test]
    fn check_parse() {
        assert_eq!(ImageProcessingParameter::parse_from_url("file://test?rotate-90-angle=1&contrast=0.5&brighten=10"), Ok(ImageProcessingParameter {
            rotate_90_angle: 1,
            contrast: 0.5,
            brighten: 10,
        }));
        // add unknown parameter
        assert_eq!(ImageProcessingParameter::parse_from_url("file://test?rotate-90-angle=1&contrast=0.5&brighten=10&unknown=10"), Ok(ImageProcessingParameter {
            rotate_90_angle: 1,
            contrast: 0.5,
            brighten: 10,
        }));
        // typo case
        assert_eq!(ImageProcessingParameter::parse_from_url("file://test?rotate_90_angle=1&contrust=0.5&brighten=10&unknown=10"), Ok(ImageProcessingParameter {
            rotate_90_angle: 0,
            contrast: 0.0,
            brighten: 10,
        }));
    }

    #[test]
    fn apply_processing_rotate() {
        let image = DynamicImage::new_rgba8(100, 200);
        let params = ImageProcessingParameter {
            rotate_90_angle: 1,
            contrast: 0.0,
            brighten: 0,
        };
        let processed = params.apply(&image).unwrap();
        assert_eq!(processed.width(), 200);
        assert_eq!(processed.height(), 100);

        let params = ImageProcessingParameter {
            rotate_90_angle: 2,
            contrast: 0.0,
            brighten: 0,
        };
        let processed = params.apply(&image).unwrap();
        assert_eq!(processed.width(), 100);
        assert_eq!(processed.height(), 200);

        let params = ImageProcessingParameter {
            rotate_90_angle: 3,
            contrast: 0.0,
            brighten: 0,
        };
        let processed = params.apply(&image).unwrap();
        assert_eq!(processed.width(), 200);
        assert_eq!(processed.height(), 100);
    }
}
