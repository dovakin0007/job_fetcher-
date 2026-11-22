use anyhow::{anyhow, Context};
use ort::{session::Session, value::Tensor};
use std::{path::Path, sync::Mutex};
use tokenizers::Tokenizer;

/// Sentence-transformer style ONNX encoder.
/// Expected graph inputs: input_ids and attention_mask; optional token_type_ids.
/// Expected output 0: token embeddings [batch, sequence, hidden].
/// Mean pooling uses the attention mask, followed by L2 normalization.
pub struct OnnxEmbedder {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    max_length: usize,
}

impl OnnxEmbedder {
    pub fn load(
        model_path: impl AsRef<Path>,
        tokenizer_path: impl AsRef<Path>,
        max_length: usize,
    ) -> anyhow::Result<Self> {
        let tokenizer = Tokenizer::from_file(tokenizer_path.as_ref())
            .map_err(|e| anyhow!("load tokenizer: {e}"))?;
        let session = Session::builder()?
            .commit_from_file(model_path.as_ref())
            .with_context(|| format!("load ONNX model {}", model_path.as_ref().display()))?;
        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            max_length,
        })
    }

    pub fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow!("tokenize input: {e}"))?;
        let ids: Vec<i64> = encoding
            .get_ids()
            .iter()
            .take(self.max_length)
            .map(|x| *x as i64)
            .collect();
        let mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .take(self.max_length)
            .map(|x| *x as i64)
            .collect();
        if ids.is_empty() {
            return Err(anyhow!("tokenizer returned an empty sequence"));
        }

        let len = ids.len();
        let ids_tensor = Tensor::from_array(([1usize, len], ids))?;
        let mask_tensor = Tensor::from_array(([1usize, len], mask.clone()))?;

        let mut session = self
            .session
            .lock()
            .map_err(|_| anyhow!("ONNX session mutex poisoned"))?;
        let inputs = ort::inputs![
            "input_ids" => ids_tensor,
            "attention_mask" => mask_tensor
        ];
        let outputs = session.run(inputs)?;
        let token_embeddings = outputs[0].try_extract_array::<f32>()?;
        let shape = token_embeddings.shape();
        if shape.len() != 3 || shape[0] != 1 {
            return Err(anyhow!(
                "expected ONNX output [1, sequence, hidden], got {shape:?}"
            ));
        }

        let hidden = shape[2];
        let mut pooled = vec![0.0f32; hidden];
        let mut count = 0.0f32;
        for token in 0..shape[1].min(mask.len()) {
            if mask[token] == 0 {
                continue;
            }
            for dim in 0..hidden {
                pooled[dim] += token_embeddings[[0, token, dim]];
            }
            count += 1.0;
        }
        if count == 0.0 {
            return Err(anyhow!("attention mask contains no active tokens"));
        }
        for value in &mut pooled {
            *value /= count;
        }

        let norm = pooled.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for value in &mut pooled {
                *value /= norm;
            }
        }
        Ok(pooled)
    }
}

pub fn cosine_similarity(a: &[f32], b: &[f32]) -> anyhow::Result<f32> {
    if a.len() != b.len() || a.is_empty() {
        return Err(anyhow!("embedding dimensions must be equal and non-empty"));
    }
    Ok(a.iter()
        .zip(b)
        .map(|(x, y)| x * y)
        .sum::<f32>()
        .clamp(-1.0, 1.0))
}
