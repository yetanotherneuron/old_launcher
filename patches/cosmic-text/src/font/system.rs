use crate::{Attrs, Font, FontMatchAttrs, HashMap, ShapeBuffer};
use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::ops::{Deref, DerefMut};
use fontdb::{FaceInfo, Family, Query, Style};
use skrifa::raw::{ReadError, TableProvider as _};
use skrifa::MetadataProvider;

// re-export fontdb and harfrust
pub use fontdb;
pub use harfrust;

use super::fallback::{Fallback, Fallbacks, MonospaceFallbackInfo, PlatformFallback};

// The fields are used in the derived Ord implementation for sorting fallback candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct FontMatchKey {
    pub(crate) not_emoji: bool,
    pub(crate) font_weight_diff: u16,
    pub(crate) font_stretch_diff: u16,
    pub(crate) font_style_diff: u8,
    pub(crate) font_weight: u16,
    pub(crate) font_stretch: u16,
    pub(crate) id: fontdb::ID,
}

impl FontMatchKey {
    // Whether a face with a different weight is a variable font covering the weight asked
    // for is not part of the key: it would mean reading every face's file. See
    // `FontSystem::matches_weight`, which reads it only for the faces it is asked about.
    fn new(attrs: &Attrs, face: &FaceInfo) -> FontMatchKey {
        // TODO: smarter way of detecting emoji
        let not_emoji = !face.post_script_name.contains("Emoji");
        let font_weight_diff = attrs.weight.0.abs_diff(face.weight.0);
        let font_weight = face.weight.0;
        let font_stretch_diff = attrs.stretch.to_number().abs_diff(face.stretch.to_number());
        let font_stretch = face.stretch.to_number();
        let font_style_diff = match (attrs.style, face.style) {
            (Style::Normal, Style::Normal)
            | (Style::Italic, Style::Italic)
            | (Style::Oblique, Style::Oblique) => 0,
            (Style::Italic, Style::Oblique) | (Style::Oblique, Style::Italic) => 1,
            (Style::Normal, Style::Italic)
            | (Style::Normal, Style::Oblique)
            | (Style::Italic, Style::Normal)
            | (Style::Oblique, Style::Normal) => 2,
        };
        let id = face.id;
        FontMatchKey {
            not_emoji,
            font_weight_diff,
            font_stretch_diff,
            font_style_diff,
            font_weight,
            font_stretch,
            id,
        }
    }
}

/// Monospace font match data for a set of attributes.
///
/// This is precomputed once per unique [`FontMatchAttrs`] by
/// [`FontSystem::get_monospace_font_matches`] so that shaping with the
/// generic `Family::Monospace` doesn't have to rescan every face in the
/// database for every word.
#[derive(Debug)]
pub struct MonoFontMatches {
    /// Match keys of all monospaced faces in the database.
    pub(crate) keys: Vec<FontMatchKey>,

    /// The default family's match key with zero weight difference (or a
    /// variable weight match), if any.
    pub(crate) default_key: Option<FontMatchKey>,

    /// Monospace fallback candidates, best first.
    ///
    /// Only populated when the `monospace_fallback` feature is disabled.
    /// In that case glyph coverage data is unavailable, so the candidate
    /// ranking doesn't depend on the word being shaped and can be reused
    /// for every word. With the feature enabled the ranking depends on the
    /// word's codepoint coverage and is cached per word by
    /// [`FontSystem::get_monospace_ranking`] instead.
    pub(crate) ranked: Vec<MonospaceFallbackInfo>,
}

struct FontCachedCodepointSupportInfo {
    supported: Vec<u32>,
    not_supported: Vec<u32>,
}

impl FontCachedCodepointSupportInfo {
    const SUPPORTED_MAX_SZ: usize = 512;
    const NOT_SUPPORTED_MAX_SZ: usize = 1024;

    fn new() -> Self {
        Self {
            supported: Vec::with_capacity(Self::SUPPORTED_MAX_SZ),
            not_supported: Vec::with_capacity(Self::NOT_SUPPORTED_MAX_SZ),
        }
    }

    #[inline(always)]
    fn unknown_has_codepoint(
        &mut self,
        font_codepoints: &[u32],
        codepoint: u32,
        supported_insert_pos: usize,
        not_supported_insert_pos: usize,
    ) -> bool {
        let ret = font_codepoints.contains(&codepoint);
        if ret {
            // don't bother inserting if we are going to truncate the entry away
            if supported_insert_pos != Self::SUPPORTED_MAX_SZ {
                self.supported.insert(supported_insert_pos, codepoint);
                self.supported.truncate(Self::SUPPORTED_MAX_SZ);
            }
        } else {
            // don't bother inserting if we are going to truncate the entry away
            if not_supported_insert_pos != Self::NOT_SUPPORTED_MAX_SZ {
                self.not_supported
                    .insert(not_supported_insert_pos, codepoint);
                self.not_supported.truncate(Self::NOT_SUPPORTED_MAX_SZ);
            }
        }
        ret
    }

    #[inline(always)]
    fn has_codepoint(&mut self, font_codepoints: &[u32], codepoint: u32) -> bool {
        match self.supported.binary_search(&codepoint) {
            Ok(_) => true,
            Err(supported_insert_pos) => match self.not_supported.binary_search(&codepoint) {
                Ok(_) => false,
                Err(not_supported_insert_pos) => self.unknown_has_codepoint(
                    font_codepoints,
                    codepoint,
                    supported_insert_pos,
                    not_supported_insert_pos,
                ),
            },
        }
    }
}

/// Access to the system fonts.
pub struct FontSystem {
    /// The locale of the system.
    locale: String,

    /// The underlying font database.
    db: fontdb::Database,

    /// Cache for loaded fonts from the database.
    font_cache: HashMap<(fontdb::ID, fontdb::Weight), Option<Arc<Font>>>,

    /// Sorted unique ID's of all Monospace fonts in DB
    monospace_font_ids: Vec<fontdb::ID>,

    /// Sorted unique ID's of all Monospace fonts in DB per script.
    /// A font may support multiple scripts of course, so the same ID
    /// may appear in multiple map value vecs.
    per_script_monospace_font_ids: HashMap<[u8; 4], Vec<fontdb::ID>>,

    /// Cache for font codepoint support info
    font_codepoint_support_info_cache: HashMap<fontdb::ID, FontCachedCodepointSupportInfo>,

    /// Cache for the range of each face's `wght` axis, `None` for a face without one,
    /// read from the face's data the first time it is needed.
    font_weight_axis_cache: HashMap<fontdb::ID, Option<(f32, f32)>>,

    /// Cache for font matches.
    font_matches_cache: HashMap<FontMatchAttrs, Arc<Vec<FontMatchKey>>>,

    /// Cache for monospace font matches.
    monospace_font_matches_cache: HashMap<FontMatchAttrs, Arc<MonoFontMatches>>,

    /// Cache for the per-word monospace fallback rankings used by the
    /// `monospace_fallback` feature path, keyed by (font match attrs, word).
    ///
    /// Only populated when the `monospace_fallback` feature is enabled;
    /// with the feature disabled the ranking is word-independent and stored
    /// in [`MonoFontMatches::ranked`] instead.
    monospace_rankings_cache:
        HashMap<(FontMatchAttrs, smol_str::SmolStr), Arc<[MonospaceFallbackInfo]>>,

    /// Scratch buffer for shaping and laying out.
    pub(crate) shape_buffer: ShapeBuffer,

    /// Buffer for use in `FontFallbackIter`.
    pub(crate) monospace_fallbacks_buffer: BTreeSet<MonospaceFallbackInfo>,

    /// Cache for shaped runs
    #[cfg(feature = "shape-run-cache")]
    pub shape_run_cache: crate::ShapeRunCache,

    /// List of fallbacks
    pub(crate) dyn_fallback: Box<dyn Fallback>,

    /// List of fallbacks
    pub(crate) fallbacks: Fallbacks,
}

impl fmt::Debug for FontSystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FontSystem")
            .field("locale", &self.locale)
            .field("db", &self.db)
            .finish_non_exhaustive()
    }
}

impl FontSystem {
    const FONT_MATCHES_CACHE_SIZE_LIMIT: usize = 256;
    /// Create a new [`FontSystem`], that allows access to any installed system fonts
    ///
    /// # Timing
    ///
    /// This function takes some time to run. On the release build, it can take up to a second,
    /// while debug builds can take up to ten times longer. For this reason, it should only be
    /// called once, and the resulting [`FontSystem`] should be shared.
    pub fn new() -> Self {
        Self::new_with_fonts(core::iter::empty())
    }

    /// Create a new [`FontSystem`] with a pre-specified set of fonts.
    pub fn new_with_fonts(fonts: impl IntoIterator<Item = fontdb::Source>) -> Self {
        let mut db = fontdb::Database::new();
        Self::load_fonts(&mut db, fonts.into_iter());
        Self::finish_with_db(db)
    }

    /// Create a new [`FontSystem`] backed by a persistent on-disk system-font index cache
    /// at the platform's conventional cache location
    /// (e.g. `$XDG_CACHE_HOME/cosmic-text/fonts.cache`).
    ///
    /// Falls back to a normal, uncached scan if no cache directory can be determined.
    #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
    pub fn new_cached() -> Self {
        Self::new_with_fonts_and_cache(core::iter::empty())
    }

    /// Returns the default system-font cache file path used by [`FontSystem::new_cached`]
    /// and [`FontSystem::new_with_fonts_and_cache`]
    /// (`<cache-dir>/cosmic-text/fonts.cache`, following the platform's conventional cache
    /// directory).
    ///
    /// Returns `None` if no cache directory can be determined from the environment.
    #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
    pub fn default_cache_path() -> Option<std::path::PathBuf> {
        super::cache::default_cache_path()
    }

    /// Like [`FontSystem::new_with_fonts`], but reads from and writes to a persistent
    /// system-font index cache at the default platform cache location (see
    /// [`FontSystem::new_cached`]).
    ///
    /// The cache path is resolved automatically; if no cache directory can be determined
    /// this falls back to a normal, uncached scan.
    /// The user-provided `fonts` are always loaded fresh and
    /// are never persisted to the cache
    ///
    /// To cache at an explicit location instead, use
    /// [`FontSystem::new_with_fonts_and_cache_path`].
    #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
    pub fn new_with_fonts_and_cache(fonts: impl IntoIterator<Item = fontdb::Source>) -> Self {
        Self::new_with_fonts_and_cache_inner(fonts, Self::default_cache_path())
    }

    /// Like [`FontSystem::new_with_fonts_and_cache`], but uses the persistent system-font
    /// index cache at the explicitly provided `cache_path` rather than the default
    /// location.
    #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
    pub fn new_with_fonts_and_cache_path(
        fonts: impl IntoIterator<Item = fontdb::Source>,
        cache_path: std::path::PathBuf,
    ) -> Self {
        Self::new_with_fonts_and_cache_inner(fonts, Some(cache_path))
    }

    #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
    fn new_with_fonts_and_cache_inner(
        fonts: impl IntoIterator<Item = fontdb::Source>,
        cache_path: Option<std::path::PathBuf>,
    ) -> Self {
        let mut db = fontdb::Database::new();

        let now = std::time::Instant::now();

        match cache_path {
            Some(cache_path) => super::cache::load_system_fonts_cached(&mut db, &cache_path),
            None => db.load_system_fonts(),
        }
        for source in fonts {
            db.load_font_source(source);
        }

        log::debug!(
            "Loaded {} font faces in {}ms.",
            db.len(),
            now.elapsed().as_millis()
        );

        Self::finish_with_db(db)
    }

    /// Apply the default font families and finish constructing the [`FontSystem`] from a
    /// loaded font database.
    fn finish_with_db(mut db: fontdb::Database) -> Self {
        let locale = Self::get_locale();
        log::debug!("Locale: {locale}");

        //TODO: configurable default fonts
        db.set_monospace_family("Noto Sans Mono");
        db.set_sans_serif_family("Open Sans");
        db.set_serif_family("DejaVu Serif");

        Self::new_with_locale_and_db_and_fallback(locale, db, PlatformFallback)
    }

    /// Create a new [`FontSystem`] with a pre-specified locale, font database and font fallback list.
    pub fn new_with_locale_and_db_and_fallback(
        locale: String,
        db: fontdb::Database,
        impl_fallback: impl Fallback + 'static,
    ) -> Self {
        let mut monospace_font_ids = db
            .faces()
            .filter(|face_info| {
                face_info.monospaced && !face_info.post_script_name.contains("Emoji")
            })
            .map(|face_info| face_info.id)
            .collect::<Vec<_>>();
        monospace_font_ids.sort();

        let mut per_script_monospace_font_ids: HashMap<[u8; 4], BTreeSet<fontdb::ID>> =
            HashMap::default();

        if cfg!(feature = "monospace_fallback") {
            for &id in &monospace_font_ids {
                db.with_face_data(id, |font_data, face_index| {
                    let face = skrifa::FontRef::from_index(font_data, face_index)?;
                    for script in face
                        .gpos()?
                        .script_list()?
                        .script_records()
                        .iter()
                        .chain(face.gsub()?.script_list()?.script_records().iter())
                    {
                        per_script_monospace_font_ids
                            .entry(script.script_tag().into_bytes())
                            .or_default()
                            .insert(id);
                    }
                    Ok::<_, ReadError>(())
                });
            }
        }

        let per_script_monospace_font_ids = per_script_monospace_font_ids
            .into_iter()
            .map(|(k, v)| (k, Vec::from_iter(v)))
            .collect();

        let fallbacks = Fallbacks::new(&impl_fallback, &[], &locale);

        Self {
            locale,
            db,
            monospace_font_ids,
            per_script_monospace_font_ids,
            font_cache: HashMap::default(),
            font_matches_cache: HashMap::default(),
            monospace_font_matches_cache: HashMap::default(),
            monospace_rankings_cache: HashMap::default(),
            font_codepoint_support_info_cache: HashMap::default(),
            font_weight_axis_cache: HashMap::default(),
            monospace_fallbacks_buffer: BTreeSet::default(),
            #[cfg(feature = "shape-run-cache")]
            shape_run_cache: crate::ShapeRunCache::default(),
            shape_buffer: ShapeBuffer::default(),
            dyn_fallback: Box::new(impl_fallback),
            fallbacks,
        }
    }

    /// Create a new [`FontSystem`] with a pre-specified locale and font database.
    pub fn new_with_locale_and_db(locale: String, db: fontdb::Database) -> Self {
        Self::new_with_locale_and_db_and_fallback(locale, db, PlatformFallback)
    }

    /// Get the locale.
    pub fn locale(&self) -> &str {
        &self.locale
    }

    /// Get the database.
    pub const fn db(&self) -> &fontdb::Database {
        &self.db
    }

    /// Get a mutable reference to the database.
    pub fn db_mut(&mut self) -> &mut fontdb::Database {
        self.font_matches_cache.clear();
        self.monospace_font_matches_cache.clear();
        self.monospace_rankings_cache.clear();
        &mut self.db
    }

    /// Consume this [`FontSystem`] and return the locale and database.
    pub fn into_locale_and_db(self) -> (String, fontdb::Database) {
        (self.locale, self.db)
    }

    /// Get a font by its ID and weight.
    pub fn get_font(&mut self, id: fontdb::ID, weight: fontdb::Weight) -> Option<Arc<Font>> {
        self.font_cache
            .entry((id, weight))
            .or_insert_with(|| {
                #[cfg(feature = "std")]
                unsafe {
                    self.db.make_shared_face_data(id);
                }
                if let Some(font) = Font::new(&self.db, id, weight) {
                    Some(Arc::new(font))
                } else {
                    log::warn!(
                        "failed to load font '{}'",
                        self.db.face(id)?.post_script_name
                    );
                    None
                }
            })
            .clone()
    }

    pub fn is_monospace(&self, id: fontdb::ID) -> bool {
        self.monospace_font_ids.binary_search(&id).is_ok()
    }

    pub fn get_monospace_ids_for_scripts(
        &self,
        scripts: impl Iterator<Item = [u8; 4]>,
    ) -> Vec<fontdb::ID> {
        let mut ret = scripts
            .filter_map(|script| self.per_script_monospace_font_ids.get(&script))
            .flat_map(|ids| ids.iter().copied())
            .collect::<Vec<_>>();
        ret.sort();
        ret.dedup();
        ret
    }

    #[inline(always)]
    pub fn get_font_supported_codepoints_in_word(
        &mut self,
        id: fontdb::ID,
        weight: fontdb::Weight,
        word: &str,
    ) -> Option<usize> {
        self.get_font(id, weight).map(|font| {
            let code_points = font.unicode_codepoints();
            let cache = self
                .font_codepoint_support_info_cache
                .entry(id)
                .or_insert_with(FontCachedCodepointSupportInfo::new);
            word.chars()
                .filter(|ch| cache.has_codepoint(code_points, u32::from(*ch)))
                .count()
        })
    }

    pub fn get_font_matches(&mut self, attrs: &Attrs<'_>) -> Arc<Vec<FontMatchKey>> {
        // Clear the cache first if it reached the size limit
        if self.font_matches_cache.len() >= Self::FONT_MATCHES_CACHE_SIZE_LIMIT {
            log::trace!("clear font mache cache");
            self.font_matches_cache.clear();
        }

        self.font_matches_cache
            //TODO: do not create AttrsOwned unless entry does not already exist
            .entry(attrs.into())
            .or_insert_with(|| {
                #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
                let now = std::time::Instant::now();

                let mut font_match_keys = self
                    .db
                    .faces()
                    .map(|face| FontMatchKey::new(attrs, face))
                    .collect::<Vec<_>>();

                // Sort so we get the keys with weight_offset=0 first
                font_match_keys.sort();

                // db.query is better than above, but returns just one font
                let query = Query {
                    families: &[attrs.family],
                    weight: attrs.weight,
                    stretch: attrs.stretch,
                    style: attrs.style,
                };

                if let Some(id) = self.db.query(&query) {
                    if let Some(i) = font_match_keys
                        .iter()
                        .enumerate()
                        .find(|(_i, key)| key.id == id)
                        .map(|(i, _)| i)
                    {
                        // if exists move to front
                        let match_key = font_match_keys.remove(i);
                        font_match_keys.insert(0, match_key);
                    } else if let Some(face) = self.db.face(id) {
                        // else insert in front
                        let match_key = FontMatchKey::new(attrs, face);
                        font_match_keys.insert(0, match_key);
                    } else {
                        log::error!("Could not get face from db, that should've been there.");
                    }
                }

                #[cfg(all(feature = "std", not(target_arch = "wasm32")))]
                {
                    let elapsed = now.elapsed();
                    log::debug!("font matches for {attrs:?} in {elapsed:?}");
                }

                Arc::new(font_match_keys)
            })
            .clone()
    }

    /// Whether the face with the given ID contains the given family name.
    pub fn face_contains_family(&self, id: fontdb::ID, family_name: &str) -> bool {
        self.db
            .face(id)
            .is_some_and(|face| face.families.iter().any(|(name, _)| name == family_name))
    }

    /// Whether the face of `m_key`, one of the [`Self::get_font_matches`] for `weight`, has
    /// that weight: as its own, or as a variable font whose `wght` axis covers it.
    ///
    /// The axis is read from the face's data the first time the face is asked about, and
    /// kept: matching reads no face's data, and a fallback reads only the faces it considers.
    pub(crate) fn matches_weight(&mut self, m_key: &FontMatchKey, weight: fontdb::Weight) -> bool {
        if m_key.font_weight_diff == 0 {
            return true;
        }

        let db = &self.db;
        let axis = *self
            .font_weight_axis_cache
            .entry(m_key.id)
            .or_insert_with(|| {
                db.with_face_data(m_key.id, |font_data, face_index| {
                    let font_ref = skrifa::FontRef::from_index(font_data, face_index).ok()?;
                    let axis = font_ref.axes().get_by_tag(skrifa::Tag::new(b"wght"))?;
                    Some((axis.min_value(), axis.max_value()))
                })
                .flatten()
            });
        let w = weight.0 as f32;
        axis.is_some_and(|(min, max)| w >= min && w <= max)
    }

    /// The first of `keys`, the [`Self::get_font_matches`] for `weight`, whose face is of
    /// `family` and has that weight (see [`Self::matches_weight`]).
    pub(crate) fn default_font_match_key(
        &mut self,
        keys: &[FontMatchKey],
        family: &Family<'_>,
        weight: fontdb::Weight,
    ) -> Option<FontMatchKey> {
        keys.iter().copied().find(|m_key| {
            self.face_contains_family(m_key.id, self.db.family_name(family))
                && self.matches_weight(m_key, weight)
        })
    }

    /// Get the cached monospace font match data for the given attributes.
    ///
    /// This precomputes, once per unique set of font match attributes, the
    /// monospace-related parts of the font matching process
    /// (see [`Self::get_font_matches`]):
    ///
    /// - [`MonoFontMatches::keys`] - match keys of all monospaced faces,
    /// - [`MonoFontMatches::default_key`] - the default family's weight
    ///   matched key (if any), and
    /// - [`MonoFontMatches::ranked`] - the fully ranked monospace fallback
    ///   candidates, when the `monospace_fallback` feature is disabled and
    ///   the ranking is therefore independent of the shaped word.
    ///
    /// Shaping with the generic `Family::Monospace` used to rescan every
    /// face in the database (an `O(faces)` coverage test) for every word,
    /// including every whitespace-only word. These results let the
    /// fallback iterator reuse the precomputed data instead.
    pub fn get_monospace_font_matches(&mut self, attrs: &Attrs<'_>) -> Arc<MonoFontMatches> {
        // Clear the cache first if it reached the size limit
        if self.monospace_font_matches_cache.len() >= Self::FONT_MATCHES_CACHE_SIZE_LIMIT {
            log::trace!("clear monospace font matches cache");
            self.monospace_font_matches_cache.clear();
        }

        let key = attrs.into();
        if let Some(matches) = self.monospace_font_matches_cache.get(&key) {
            return matches.clone();
        }

        let font_match_keys = self.get_font_matches(attrs);
        let default_key =
            self.default_font_match_key(&font_match_keys, &attrs.family, attrs.weight);

        let mut keys = Vec::new();
        for m_key in font_match_keys.iter() {
            if self.is_monospace(m_key.id) {
                keys.push(*m_key);
            }
        }

        // When the `monospace_fallback` feature is disabled, glyph coverage
        // data is unavailable and the codepoint ranking is identical for
        // every word, so the full ranking can be precomputed here: the
        // default family's match (if any) sorts first (its
        // `font_weight_diff` is `None`), and the remaining candidates are
        // ordered by (weight difference, weight, id).
        let mut ranked = Vec::new();
        if !cfg!(feature = "monospace_fallback") {
            if let Some(default_key) = default_key {
                ranked.push(MonospaceFallbackInfo {
                    font_weight_diff: None,
                    codepoint_non_matches: None,
                    font_weight: default_key.font_weight,
                    id: default_key.id,
                });
            }
            let mut others: Vec<&FontMatchKey> = keys
                .iter()
                .filter(|m_key| !matches!(default_key, Some(ref dk) if dk.id == m_key.id))
                .collect();
            others.sort_by_key(|m_key| (m_key.font_weight_diff, m_key.font_weight, m_key.id));
            ranked.extend(others.iter().map(|m_key| MonospaceFallbackInfo {
                font_weight_diff: Some(m_key.font_weight_diff),
                codepoint_non_matches: None,
                font_weight: m_key.font_weight,
                id: m_key.id,
            }));
        }

        let matches = Arc::new(MonoFontMatches {
            keys,
            default_key,
            ranked,
        });
        self.monospace_font_matches_cache
            .insert(key, matches.clone());
        matches
    }

    /// Get the cached per-word monospace fallback ranking for the given
    /// attributes and word.
    ///
    /// The ranking (best candidate first, see [`MonospaceFallbackInfo`])
    /// depends on the word's codepoint coverage, so - unlike
    /// [`Self::get_monospace_font_matches`] - it is cached per word as well.
    /// The same word is shaped repeatedly (e.g. every whitespace run in a
    /// terminal UI), so the per-word cache hits most of the time.
    ///
    /// If the default monospace font covers every codepoint of the word,
    /// the ranking is truncated to that font alone: the other candidates
    /// could not rank above it, and skipping their coverage checks is what
    /// keeps fully-covered words cheap.
    ///
    /// This is only used by the `monospace_fallback` feature path; with the
    /// feature disabled the word-independent ranking is available from
    /// [`MonoFontMatches::ranked`].
    pub fn get_monospace_ranking(
        &mut self,
        attrs: &Attrs<'_>,
        mono: &MonoFontMatches,
        word: &str,
        scripts: &[unicode_script::Script],
    ) -> Arc<[MonospaceFallbackInfo]> {
        // Clear the cache first if it reached the size limit
        if self.monospace_rankings_cache.len() >= Self::FONT_MATCHES_CACHE_SIZE_LIMIT {
            log::trace!("clear monospace rankings cache");
            self.monospace_rankings_cache.clear();
        }

        let key = (attrs.into(), word.into());
        if let Some(ranking) = self.monospace_rankings_cache.get(&key) {
            return ranking.clone();
        }

        let mono_ids_for_scripts =
            self.get_monospace_ids_for_scripts(scripts.iter().filter_map(|script| {
                let script_as_lower = script.short_name().to_lowercase();
                <[u8; 4]>::try_from(script_as_lower.as_bytes()).ok()
            }));

        let word_chars_count = word.chars().count();
        let mut ranking = Vec::new();
        if let Some(default_key) = mono.default_key {
            if let Some(supported_cp_count) =
                self.get_font_supported_codepoints_in_word(default_key.id, attrs.weight, word)
            {
                let codepoint_non_matches = word_chars_count - supported_cp_count;
                ranking.push(MonospaceFallbackInfo {
                    font_weight_diff: None,
                    codepoint_non_matches: Some(codepoint_non_matches),
                    font_weight: default_key.font_weight,
                    id: default_key.id,
                });
                if codepoint_non_matches == 0 {
                    // The default Monospace font supports all word codepoints:
                    // return it alone, like the non-mono fast path.
                    let arc: Arc<[MonospaceFallbackInfo]> = Arc::from(ranking);
                    self.monospace_rankings_cache.insert(key, arc.clone());
                    return arc;
                }
            }
        }

        for m_key in mono.keys.iter() {
            if Some(m_key.id) == mono.default_key.map(|dk| dk.id) {
                continue;
            }
            if !mono_ids_for_scripts.is_empty()
                && mono_ids_for_scripts.binary_search(&m_key.id).is_err()
            {
                continue;
            }
            if let Some(supported_cp_count) =
                self.get_font_supported_codepoints_in_word(m_key.id, attrs.weight, word)
            {
                ranking.push(MonospaceFallbackInfo {
                    font_weight_diff: Some(m_key.font_weight_diff),
                    codepoint_non_matches: Some(word_chars_count - supported_cp_count),
                    font_weight: m_key.font_weight,
                    id: m_key.id,
                });
            }
        }

        ranking.sort();
        let arc: Arc<[MonospaceFallbackInfo]> = Arc::from(ranking);
        self.monospace_rankings_cache.insert(key, arc.clone());
        arc
    }

    #[cfg(feature = "std")]
    fn get_locale() -> String {
        sys_locale::get_locale().unwrap_or_else(|| {
            log::warn!("failed to get system locale, falling back to en-US");
            String::from("en-US")
        })
    }

    #[cfg(not(feature = "std"))]
    fn get_locale() -> String {
        String::from("en-US")
    }

    #[cfg(feature = "std")]
    fn load_fonts(db: &mut fontdb::Database, fonts: impl Iterator<Item = fontdb::Source>) {
        #[cfg(not(target_arch = "wasm32"))]
        let now = std::time::Instant::now();

        db.load_system_fonts();

        for source in fonts {
            db.load_font_source(source);
        }

        #[cfg(not(target_arch = "wasm32"))]
        log::debug!(
            "Parsed {} font faces in {}ms.",
            db.len(),
            now.elapsed().as_millis()
        );
    }

    #[cfg(not(feature = "std"))]
    fn load_fonts(db: &mut fontdb::Database, fonts: impl Iterator<Item = fontdb::Source>) {
        for source in fonts {
            db.load_font_source(source);
        }
    }
}

/// A value borrowed together with an [`FontSystem`]
#[derive(Debug)]
pub struct BorrowedWithFontSystem<'a, T> {
    pub(crate) inner: &'a mut T,
    pub(crate) font_system: &'a mut FontSystem,
}

impl<T> Deref for BorrowedWithFontSystem<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.inner
    }
}

impl<T> DerefMut for BorrowedWithFontSystem<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner
    }
}

#[cfg(test)]
mod test {
    use crate::{Attrs, Family, FontSystem, Weight};

    /// A variable font has every weight its `wght` axis covers, a static one only its own.
    /// Matching reads no face's data, and finding the default family's face reads only the
    /// faces of that family.
    #[test]
    fn a_variable_font_has_every_weight_of_its_axis() {
        let fonts = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fonts");
        let mut db = fontdb::Database::new();
        for font in ["Inter-Regular.ttf", "InterVariable.ttf"] {
            db.load_font_data(std::fs::read(fonts.join(font)).unwrap());
        }
        let mut font_system = FontSystem::new_with_locale_and_db("en-US".into(), db);
        let face = |family: &str| {
            font_system
                .db()
                .faces()
                .find(|face| face.families[0].0 == family)
                .unwrap()
                .id
        };
        let (inter, variable) = (face("Inter"), face("Inter Variable"));

        let cases = [
            ("Inter Variable", 100, Some(variable)),
            ("Inter Variable", 700, Some(variable)),
            ("Inter Variable", 900, Some(variable)),
            ("Inter Variable", 950, None),
            ("Inter", 400, Some(inter)),
            ("Inter", 700, None),
        ];
        let matches: Vec<_> = cases
            .iter()
            .map(|(family, weight, _)| {
                let attrs = Attrs::new()
                    .family(Family::Name(family))
                    .weight(Weight(*weight));
                font_system.get_font_matches(&attrs)
            })
            .collect();
        assert!(font_system.font_weight_axis_cache.is_empty());

        for ((family, weight, expected), keys) in cases.iter().zip(&matches) {
            let found = font_system
                .default_font_match_key(keys, &Family::Name(family), Weight(*weight))
                .map(|key| key.id);
            assert_eq!(found, *expected, "{family} at {weight}");
            if *family == "Inter Variable" {
                assert!(!font_system.font_weight_axis_cache.contains_key(&inter));
            }
        }
    }
}
