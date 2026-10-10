//! The BERT encoders on their real weights. Ignored by default (they need
//! ~220 MB of checkpoints); run with
//! `FLINCH_EMBED_TEST_MODELS=<dir> cargo test -p flinch-archive --test embed_models -- --include-ignored`,
//! `<dir>` holding `all-MiniLM-L6-v2/` and `bge-small-en-v1.5/`, each with
//! `config.json`, `tokenizer.json` and `model.safetensors`.

use flinch_archive::embedding::{BertEncoder, EmbeddingEngine, Pooling};
use rstest::rstest;

const SCI_FI: [&str; 5] = [
    "Title: The Expanse\nKind: series\nGenres: Drama, Science Fiction\nOverview: Two hundred years from now, humanity has colonised the solar system; a detective and a ship's crew uncover a conspiracy that threatens Earth, Mars and the Belt.",
    "Title: Battlestar Galactica\nKind: series\nGenres: Action, Science Fiction\nOverview: The last survivors of humanity flee across space in a ragtag fleet, hunted by the Cylons, machines that can look human.",
    "Title: Star Trek: The Next Generation\nKind: series\nGenres: Adventure, Science Fiction\nOverview: The crew of the starship Enterprise explores strange new worlds and seeks out new life and new civilisations.",
    "Title: Foundation\nKind: series\nGenres: Drama, Science Fiction\nOverview: A mathematician predicts the fall of the Galactic Empire and gathers exiles on a distant planet to shorten the dark age to come.",
    "Title: Babylon 5\nKind: series\nGenres: Drama, Science Fiction\nOverview: On a space station in neutral territory, diplomats of alien empires and humans negotiate as an interstellar war looms.",
];
const HELD_OUT_SCI_FI: &str = "Title: Firefly\nKind: series\nGenres: Western, Science Fiction\nOverview: Five hundred years in the future, the captain of a small transport spaceship takes any job to keep his crew flying among the outer planets.";
const COOKING: [&str; 4] = [
    "Title: The Great British Bake Off\nKind: series\nGenres: Reality\nOverview: Amateur bakers compete in a tent in the English countryside, baking cakes, breads and pastries for two judges.",
    "Title: MasterChef\nKind: series\nGenres: Reality, Food\nOverview: Home cooks compete in kitchen challenges, cooking dishes for professional chefs who eliminate one each week.",
    "Title: Barefoot Contessa\nKind: series\nGenres: Food\nOverview: A cook shares simple recipes for elegant dinners, roast chicken and desserts from her kitchen in the Hamptons.",
    "Title: Salt Fat Acid Heat\nKind: series\nGenres: Documentary, Food\nOverview: A chef travels to Italy, Japan, Mexico and California to show the four elements of good cooking.",
];

/// The encoder, or `None` (the test passes vacuously) without the env var.
fn encoder(name: &str, pooling: Pooling) -> Option<BertEncoder> {
    let dir = std::env::var_os("FLINCH_EMBED_TEST_MODELS")?;
    Some(BertEncoder::open(&std::path::Path::new(&dir).join(name), pooling).expect("the checkpoint opens"))
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[rstest]
#[case::minilm("all-MiniLM-L6-v2", Pooling::Mean)]
#[case::bge("bge-small-en-v1.5", Pooling::Cls)]
#[ignore = "needs FLINCH_EMBED_TEST_MODELS"]
fn every_vector_is_384_d_and_unit_length(#[case] name: &str, #[case] pooling: Pooling) {
    let Some(encoder) = encoder(name, pooling) else { return };
    assert_eq!(encoder.dimension(), 384);
    let texts: Vec<&str> = SCI_FI.iter().chain(&COOKING).copied().chain(["x", HELD_OUT_SCI_FI]).collect();
    let vectors = encoder.embed(&texts).expect("embedded");
    assert_eq!(vectors.len(), texts.len());
    for vector in &vectors {
        assert_eq!(vector.len(), 384);
        let norm = vector.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>().sqrt();
        assert!((norm - 1.0).abs() <= 1e-5, "norm {norm}");
    }
}

#[rstest]
#[case::minilm("all-MiniLM-L6-v2", Pooling::Mean)]
#[case::bge("bge-small-en-v1.5", Pooling::Cls)]
#[ignore = "needs FLINCH_EMBED_TEST_MODELS"]
fn a_text_embeds_the_same_alone_and_padded_in_a_batch(#[case] name: &str, #[case] pooling: Pooling) {
    let Some(encoder) = encoder(name, pooling) else { return };
    let short = "Title: Alien\nKind: movie";
    let alone = encoder.embed(&[short]).expect("embedded").remove(0);
    let batched = encoder.embed(&[short, SCI_FI[0]]).expect("embedded").remove(0);
    let cosine = dot(&alone, &batched);
    assert!(cosine > 0.9999, "cosine {cosine}");
}

#[rstest]
#[case::minilm("all-MiniLM-L6-v2", Pooling::Mean)]
#[case::bge("bge-small-en-v1.5", Pooling::Cls)]
#[ignore = "needs FLINCH_EMBED_TEST_MODELS"]
fn a_sci_fi_centroid_prefers_held_out_sci_fi_to_every_cooking_show(#[case] name: &str, #[case] pooling: Pooling) {
    let Some(encoder) = encoder(name, pooling) else { return };
    let sci_fi = encoder.embed(&SCI_FI).expect("embedded");
    let mut centroid = vec![0.0f32; 384];
    for vector in &sci_fi {
        centroid.iter_mut().zip(vector).for_each(|(sum, x)| *sum += x);
    }
    let held_out = dot(&centroid, &encoder.embed(&[HELD_OUT_SCI_FI]).expect("embedded")[0]);
    for (show, vector) in COOKING.iter().zip(encoder.embed(&COOKING).expect("embedded")) {
        let cooking = dot(&centroid, &vector);
        assert!(held_out > cooking, "{held_out} <= {cooking} for {show}");
    }
}
