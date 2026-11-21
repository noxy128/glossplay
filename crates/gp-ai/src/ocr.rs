//! OCR (Optical Character Recognition) for extracting text from video frames.
//! Uses RapidOCR (ONNX models) — fast, local, no API keys.

use crate::AiError;
use gp_core::config::Ocr as OcrConfig;
use image::{DynamicImage, ImageBuffer, Rgba};
use ndarray::{Array4, CowArray, s};
use ort::{GraphOptimizationLevel, Session, SessionBuilder};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

// RapidOCR model URLs (hosted on Hugging Face)
const DET_MODEL_URL: &str =
    "https://huggingface.co/RapidAI/RapidOCR/resolve/main/onnx/det/ch_PP-OCRv4_det_infer.onnx";
const REC_MODEL_URL: &str =
    "https://huggingface.co/RapidAI/RapidOCR/resolve/main/onnx/rec/ch_ppocr_mobile_v2.0_rec_infer.onnx";
const REC_LABEL_URL: &str =
    "https://huggingface.co/RapidAI/RapidOCR/resolve/main/onnx/rec/ppocr_keys_v1.txt";

/// OCR result: text with position and confidence.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrResult {
    /// Recognized text.
    pub text: String,
    /// Bounding box in image pixels: (x, y, width, height).
    pub bbox: (i32, i32, i32, i32),
    /// Average confidence 0-100.
    pub confidence: u8,
}

/// High-level OCR engine wrapper using ONNX Runtime.
pub struct OcrEngine {
    det_session: Session,
    rec_session: Session,
    rec_labels: Vec<String>,
    confidence_threshold: u8,
}

impl OcrEngine {
    /// Create a new OCR engine from config.
    pub fn new(config: &OcrConfig) -> Result<Self, AiError> {
        let model_dir = Self::model_dir(config);
        std::fs::create_dir_all(&model_dir).map_err(|e| {
            AiError::Config(format!("create model dir failed: {e}"))
        })?;

        let det_path = Self::ensure_model(&model_dir, "det.onnx", DET_MODEL_URL)?;
        let rec_path = Self::ensure_model(&model_dir, "rec.onnx", REC_MODEL_URL)?;
        let labels_path = Self::ensure_model(&model_dir, "labels.txt", REC_LABEL_URL)?;

        let det_session = SessionBuilder::new()
            .map_err(|e| AiError::Config(format!("ORT session builder failed: {e}")))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| AiError::Config(format!("ORT optimization failed: {e}")))?
            .with_intra_threads(4)
            .map_err(|e| AiError::Config(format!("ORT threads failed: {e}")))?
            .commit_from_file(&det_path)
            .map_err(|e| AiError::Config(format!("load det model failed: {e}")))?;

        let rec_session = SessionBuilder::new()
            .map_err(|e| AiError::Config(format!("ORT session builder failed: {e}")))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| AiError::Config(format!("ORT optimization failed: {e}")))?
            .with_intra_threads(4)
            .map_err(|e| AiError::Config(format!("ORT threads failed: {e}")))?
            .commit_from_file(&rec_path)
            .map_err(|e| AiError::Config(format!("load rec model failed: {e}")))?;

        let rec_labels = std::fs::read_to_string(&labels_path)
            .map_err(|e| AiError::Config(format!("read labels failed: {e}")))?
            .lines()
            .map(|s| s.to_string())
            .collect();

        Ok(OcrEngine {
            det_session,
            rec_session,
            rec_labels,
            confidence_threshold: config.confidence_threshold,
        })
    }

    fn model_dir(config: &OcrConfig) -> PathBuf {
        if !config.model_path.is_empty() {
            PathBuf::from(&config.model_path)
        } else {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("glossplay")
                .join("ocr_models")
        }
    }

    fn ensure_model(model_dir: &Path, name: &str, url: &str) -> Result<PathBuf, AiError> {
        let path = model_dir.join(name);
        if path.exists() {
            return Ok(path);
        }
        eprintln!("Downloading OCR model: {url}");
        let bytes = ureq::get(url)
            .call()
            .map_err(|e| AiError::Network(format!("download failed: {e}")))?
            .into_body()
            .read_to_vec()
            .map_err(|e| AiError::Network(format!("read failed: {e}")))?;
        std::fs::write(&path, &bytes)
            .map_err(|e| AiError::Config(format!("write model failed: {e}")))?;
        Ok(path)
    }

    /// Run OCR on raw RGBA pixels (from video frame).
    pub fn recognize_rgba(
        &mut self,
        width: u32,
        height: u32,
        data: &[u8],
    ) -> Result<Vec<OcrResult>, AiError> {
        // Convert RGBA to RGB
        let img = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, data.to_vec())
            .ok_or_else(|| AiError::Config("invalid frame dimensions".into()))?;
        let rgb_img = DynamicImage::ImageRgba8(img).to_rgb8();

        // Detect text boxes
        let boxes = self.detect_text(&rgb_img)?;

        // Recognize text in each box
        let mut results = Vec::new();
        for box_pts in boxes {
            if let Some(result) = self.recognize_crop(&rgb_img, &box_pts)? {
                if result.confidence >= self.confidence_threshold {
                    results.push(result);
                }
            }
        }

        // Sort by Y then X for reading order
        results.sort_by(|a, b| {
            a.bbox.1.cmp(&b.bbox.1).then(a.bbox.0.cmp(&b.bbox.0))
        });

        Ok(results)
    }

    /// Detect text regions using DBNet.
    fn detect_text(&self, img: &DynamicImage) -> Result<Vec<Vec<(f32, f32)>>, AiError> {
        let (input, ratio_w, ratio_h) = preprocess_det(img);
        let input_array = Array4::from_shape_vec((1, 3, input.height() as usize, input.width() as usize), input)
            .map_err(|e| AiError::Config(format!("det input shape failed: {e}")))?;

        let outputs = self.det_session.run(ort::inputs!["x" => input_array.view()])
            .map_err(|e| AiError::Config(format!("det inference failed: {e}")))?;

        let prob_map = outputs["maps"].try_extract_tensor::<f32>()
            .map_err(|e| AiError::Config(format!("det output extract failed: {e}")))?;
        let prob_map = prob_map.slice(s![0, 0, .., ..]);

        // Simple threshold + contour finding (simplified)
        let boxes = postprocess_det(&prob_map, ratio_w, ratio_h, img.width() as f32, img.height() as f32);
        Ok(boxes)
    }

    /// Recognize text in a cropped region.
    fn recognize_crop(
        &self,
        img: &DynamicImage,
        box_pts: &[(f32, f32)],
    ) -> Result<Option<OcrResult>, AiError> {
        // Get crop bounds
        let (min_x, min_y, max_x, max_y) = box_pts.iter().fold(
            (f32::MAX, f32::MAX, f32::MIN, f32::MIN),
            |(min_x, min_y, max_x, max_y), &(x, y)| {
                (min_x.min(x), min_y.min(y), max_x.max(x), max_y.max(y))
            },
        );

        let w = (max_x - min_x).max(1.0) as u32;
        let h = (max_y - min_y).max(1.0) as u32;
        if w < 10 || h < 10 {
            return Ok(None);
        }

        // Crop and resize to 320x32 for recognition
        let crop = img.crop_imm(min_x as u32, min_y as u32, w, h);
        let resized = crop.resize_exact(320, 32, image::imageops::FilterType::Triangle);
        let rgb = resized.to_rgb8();

        // Normalize to [-1, 1] as expected by the model
        let mut input_data = Vec::with_capacity(3 * 32 * 320);
        for c in 0..3 {
            for y in 0..32 {
                for x in 0..320 {
                    let pixel = rgb.get_pixel(x, y)[c] as f32 / 255.0;
                    input_data.push((pixel - 0.5) / 0.5);
                }
            }
        }

        let input_array = Array4::from_shape_vec((1, 3, 32, 320), input_data)
            .map_err(|e| AiError::Config(format!("rec input shape failed: {e}")))?;

        let outputs = self.rec_session.run(ort::inputs!["x" => input_array.view()])
            .map_err(|e| AiError::Config(format!("rec inference failed: {e}")))?;

        let logits = outputs["out"].try_extract_tensor::<f32>()
            .map_err(|e| AiError::Config(format!("rec output extract failed: {e}")))?;
        let logits = logits.slice(s![0, .., ..]);

        // CTC greedy decode
        let (text, conf) = ctc_decode(&logits, &self.rec_labels)?;

        Ok(Some(OcrResult {
            text,
            bbox: (min_x as i32, min_y as i32, w as i32, h as i32),
            confidence: (conf * 100.0) as u8,
        }))
    }
}

/// Preprocess image for DBNet detection.
fn preprocess_det(img: &DynamicImage) -> (ImageBuffer<image::Rgb<f32>, Vec<f32>>, f32, f32) {
    let max_side = 960;
    let (w, h) = (img.width() as f32, img.height() as f32);
    let scale = max_side as f32 / w.max(h);
    let new_w = (w * scale / 32.0).ceil() * 32.0;
    let new_h = (h * scale / 32.0).ceil() * 32.0;

    let resized = img.resize_exact(new_w as u32, new_h as u32, image::imageops::FilterType::Triangle);
    let rgb = resized.to_rgb8();

    let mut data = Vec::with_capacity(3 * new_w as usize * new_h as usize);
    for c in 0..3 {
        for y in 0..new_h as usize {
            for x in 0..new_w as usize {
                let pixel = rgb.get_pixel(x as u32, y as u32)[c] as f32 / 255.0;
                data.push((pixel - 0.5) / 0.5); // Normalize to [-1, 1]
            }
        }
    }

    let input = ImageBuffer::from_raw(new_w as u32, new_h as u32, data)
        .expect("preprocess data size mismatch");
    (input, w / new_w, h / new_h)
}

/// Postprocess detection output to get text boxes.
fn postprocess_det(
    prob_map: &ndarray::ArrayView2<f32>,
    ratio_w: f32,
    ratio_h: f32,
    orig_w: f32,
    orig_h: f32,
) -> Vec<Vec<(f32, f32)>> {
    let (h, w) = prob_map.dim();
    let threshold = 0.3;
    let mut boxes = Vec::new();

    // Simplified: find connected components above threshold
    // In practice, this would use OpenCV's findContours + minAreaRect
    // Here we use a simple grid-based approach for demo
    let step = 4;
    for y in (0..h).step_by(step) {
        for x in (0..w).step_by(step) {
            if prob_map[[y, x]] > threshold {
                // Found a potential text region, expand to get box
                let box_w = (w as f32 * 0.1).max(50.0) as usize;
                let box_h = (h as f32 * 0.05).max(20.0) as usize;
                let x1 = (x as f32 * ratio_w).clamp(0.0, orig_w);
                let y1 = (y as f32 * ratio_h).clamp(0.0, orig_h);
                let x2 = ((x + box_w) as f32 * ratio_w).clamp(0.0, orig_w);
                let y2 = ((y + box_h) as f32 * ratio_h).clamp(0.0, orig_h);

                if x2 > x1 + 10.0 && y2 > y1 + 10.0 {
                    boxes.push(vec![
                        (x1, y1),
                        (x2, y1),
                        (x2, y2),
                        (x1, y2),
                    ]);
                }
            }
        }
    }

    // Non-maximum suppression (simplified)
    nms(boxes, 0.3)
}

/// Non-maximum suppression for boxes.
fn nms(mut boxes: Vec<Vec<(f32, f32)>>, iou_threshold: f32) -> Vec<Vec<(f32, f32)>> {
    boxes.sort_by(|a, b| {
        let area_a = box_area(a);
        let area_b = box_area(b);
        area_b.partial_cmp(&area_a).unwrap()
    });

    let mut keep = Vec::new();
    for box_a in boxes {
        let mut suppressed = false;
        for box_b in &keep {
            if iou(&box_a, box_b) > iou_threshold {
                suppressed = true;
                break;
            }
        }
        if !suppressed {
            keep.push(box_a);
        }
    }
    keep
}

fn box_area(box_pts: &[(f32, f32)]) -> f32 {
    if box_pts.len() < 4 {
        return 0.0;
    }
    let (x1, y1) = box_pts[0];
    let (x2, y2) = box_pts[2];
    (x2 - x1).abs() * (y2 - y1).abs()
}

fn iou(a: &[(f32, f32)], b: &[(f32, f32)]) -> f32 {
    if a.len() < 4 || b.len() < 4 {
        return 0.0;
    }
    let (ax1, ay1) = a[0];
    let (ax2, ay2) = a[2];
    let (bx1, by1) = b[0];
    let (bx2, by2) = b[2];

    let ix1 = ax1.max(bx1);
    let iy1 = ay1.max(by1);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);

    if ix2 <= ix1 || iy2 <= iy1 {
        return 0.0;
    }

    let inter = (ix2 - ix1) * (iy2 - iy1);
    let area_a = (ax2 - ax1) * (ay2 - ay1);
    let area_b = (bx2 - bx1) * (by2 - by1);
    inter / (area_a + area_b - inter)
}

/// CTC greedy decode for recognition output.
fn ctc_decode(
    logits: &ndarray::ArrayView2<f32>,
    labels: &[String],
) -> Result<(String, f32), AiError> {
    let (seq_len, num_classes) = logits.dim();
    let blank = num_classes - 1; // Last class is usually blank

    let mut text = String::new();
    let mut conf_sum = 0.0;
    let mut conf_count = 0;
    let mut prev = blank;

    for t in 0..seq_len {
        // Find max probability class
        let mut max_idx = 0;
        let mut max_val = f32::NEG_INFINITY;
        for c in 0..num_classes {
            if logits[[t, c]] > max_val {
                max_val = logits[[t, c]];
                max_idx = c;
            }
        }

        if max_idx != blank && max_idx != prev {
            if max_idx < labels.len() {
                text.push_str(&labels[max_idx]);
            }
            conf_sum += max_val;
            conf_count += 1;
        }
        prev = max_idx;
    }

    let conf = if conf_count > 0 {
        conf_sum / conf_count as f32
    } else {
        0.0
    };

    Ok((text, conf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ctc_decode() {
        let labels: Vec<String> = (0..66).map(|i| char::from_u32(i + 32).unwrap().to_string()).collect();
        let logits = ndarray::Array2::zeros((10, 67));
        let (text, conf) = ctc_decode(&logits.view(), &labels).unwrap();
        assert_eq!(text, "");
        assert_eq!(conf, 0.0);
    }
}