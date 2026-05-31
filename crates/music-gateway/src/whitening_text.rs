//! Cross-modal text-mean fitting for ABTT whitening.
//!
//! The whitening transform is fit on audio embeddings, but CLaMP 3 text
//! embeddings sit at an offset (the modality gap). To keep station text
//! queries comparable to the stored audio vectors we estimate the
//! text-modality mean `μ_text` by embedding a fixed corpus of
//! representative music descriptors through the sidecar and averaging.
//! That mean is attached to the [`Whitening`](music_recommend::Whitening)
//! transform and used to center text queries instead of the audio mean.
//!
//! The corpus only needs to *span* the kinds of phrases users type into a
//! station box — genres, moods, instruments, tempo, era, activity — so the
//! centroid lands in the right region of text space. It does not need to
//! be exhaustive; a few-hundred-ms-each embed of ~100 prompts (once, then
//! cached in `embedding_whitening.text_mean`) is enough.

use music_recommend::EmbedderClient;

/// Representative station-style prompts spanning the text-query
/// distribution. Averaged in text-embedding space to estimate `μ_text`.
pub const TEXT_PROMPT_CORPUS: &[&str] = &[
    // Genres
    "heavy metal",
    "thrash metal",
    "death metal",
    "black metal",
    "punk rock",
    "classic rock",
    "indie rock",
    "alternative rock",
    "hard rock",
    "progressive rock",
    "pop music",
    "synth pop",
    "dance pop",
    "hip hop",
    "boom bap rap",
    "trap beats",
    "rhythm and blues",
    "soul music",
    "funk groove",
    "jazz",
    "smooth jazz",
    "bebop jazz",
    "blues",
    "country music",
    "folk music",
    "acoustic singer songwriter",
    "classical music",
    "baroque orchestral",
    "piano concerto",
    "ambient electronic",
    "techno",
    "house music",
    "deep house",
    "drum and bass",
    "dubstep",
    "trance",
    "lo-fi beats",
    "reggae",
    "ska",
    "disco",
    "gospel choir",
    "world music",
    "latin jazz",
    "bossa nova",
    "flamenco guitar",
    "electronic dance music",
    "shoegaze",
    "post rock",
    "math rock",
    "grunge",
    // Moods
    "happy and upbeat",
    "sad and melancholic",
    "angry and aggressive",
    "calm and relaxing",
    "energetic and intense",
    "dreamy and atmospheric",
    "dark and brooding",
    "romantic and tender",
    "nostalgic",
    "triumphant and epic",
    "chill and mellow",
    "anxious and tense",
    "uplifting and hopeful",
    "groovy and danceable",
    "haunting and eerie",
    // Instruments / texture
    "distorted electric guitar",
    "acoustic guitar fingerpicking",
    "heavy bass and drums",
    "soaring strings",
    "solo piano",
    "saxophone solo",
    "synthesizer pads",
    "vocal harmonies",
    "a cappella",
    "orchestral film score",
    // Tempo / energy
    "fast and frantic",
    "slow and brooding",
    "mid-tempo groove",
    "driving rhythm",
    // Era
    "1960s psychedelic",
    "1970s funk",
    "1980s synthwave",
    "1990s alternative",
    "2000s pop punk",
    // Activity / scene
    "rainy sunday afternoon",
    "late night drive",
    "workout motivation",
    "study focus",
    "summer beach party",
    "cozy coffee shop",
    "morning wake up",
    "intense gym session",
    "winding down before sleep",
    "road trip anthems",
];

/// Embed the prompt corpus and return the mean vector (`μ_text`).
///
/// Each `embed_text` result is already L2-normalized; we average them in
/// f64 (centroid of unit vectors — intentionally *not* renormalized, since
/// this is a mean to subtract, not a query). Returns an error if the
/// embedder fails on every prompt or the dimensionality doesn't match.
pub async fn fit_text_mean(client: &EmbedderClient, dim: usize) -> Result<Vec<f32>, String> {
    let mut acc = vec![0.0_f64; dim];
    let mut n = 0usize;
    let mut last_err: Option<String> = None;
    for prompt in TEXT_PROMPT_CORPUS {
        match client.embed_text(prompt).await {
            Ok(res) => {
                if res.vector.len() != dim {
                    last_err = Some(format!(
                        "embed_text returned dim {} != expected {dim}",
                        res.vector.len()
                    ));
                    continue;
                }
                for (a, &x) in acc.iter_mut().zip(&res.vector) {
                    *a += f64::from(x);
                }
                n += 1;
            }
            Err(e) => last_err = Some(e.to_string()),
        }
    }
    if n == 0 {
        return Err(last_err.unwrap_or_else(|| "no prompts embedded".to_string()));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
    let mean: Vec<f32> = acc.iter().map(|&a| (a / n as f64) as f32).collect();
    Ok(mean)
}
