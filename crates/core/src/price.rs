//! The price book (docs/ledger.md; docs/cloudflare-v1.md, decisions 24,
//! 25 and 37, spike S4): what each meter costs at Cloudflare's list
//! price, and what a person is charged for it.
//!
//! A price is micro-dollars per a fixed count of its unit (a million
//! tokens, an awake hour, a GB-month), so every price in Cloudflare's own
//! tables is an exact integer here. A row's charge is
//! `ceil(list × (1 + fee) × (1 + margin))`, computed exactly in 128 bits
//! and rounded up once, never per part. The fee is AI Gateway's 5% on the
//! credits that pay for AI through Unified Billing (spike S4), so it
//! applies to tokens and neurons only; the margin is the operator's, 50%
//! unless the book sets another. The book is data the operator sets (the
//! deploy's configuration) and versions; a ledger keeps the version it
//! charges with (crate::ledger).

use serde::{Deserialize, Serialize};

/// One US dollar in micro-dollars.
pub const USD: i64 = 1_000_000;
/// Basis points in a whole: 10,000 bp is 100%.
pub const BP: u32 = 10_000;
/// The operator's margin when the configuration names none (decision 24).
pub const MARGIN_BP_DEFAULT: u32 = 5_000;
/// AI Gateway's fee on the credits that pay for AI (spike S4): AI's cost
/// basis is its list price × 1.05.
pub const CREDITS_FEE_BP_DEFAULT: u32 = 500;
/// A margin above 10× or a fee above 100% is a typo in a configuration,
/// not a price.
pub const MARGIN_BP_MAX: u32 = 100_000;
pub const FEE_BP_MAX: u32 = 10_000;
/// One price, in micro-dollars per its unit's count: $1M a million tokens
/// (or an awake hour) is far past anything Cloudflare lists.
pub const PRICE_MAX: i64 = 1_000_000 * USD;
/// One quantity of a usage: 10^15 tokens, ms or byte-hours is far past
/// any one row (a 20 GB disk for a month is 1.4 × 10^13 byte-hours), and
/// the bound keeps the 128-bit arithmetic below from ever overflowing.
pub const QUANTITY_MAX: u64 = 1_000_000_000_000_000;
/// One row's charge: no single call or sample costs $100,000, so a row
/// that would is a bug upstream, refused rather than billed.
pub const CHARGE_MAX: i64 = 100_000 * USD;
/// Entries in each of a book's lists (models, instances, keys).
pub const PRICES_MAX: usize = 256;
/// A model's, an instance type's or a key's name.
pub const NAME_MAX_BYTES: usize = 128;

/// The largest numerator `price` forms: four token classes at their
/// limits, times the largest fee and margin factors.
const NUMERATOR_MAX: i128 = 4 * (QUANTITY_MAX as i128) * (PRICE_MAX as i128) * ((BP + FEE_BP_MAX) as i128) * ((BP + MARGIN_BP_MAX) as i128);
const _: () = assert!(NUMERATOR_MAX < i128::MAX / 2, "a charge's exact numerator fits 128 bits with room");
const _: () = assert!(CHARGE_MAX <= i64::MAX / 1024, "a ledger sums many charges in an i64");

/// Tokens are priced per million (the catalogs' unit).
pub const TOKENS_PER: u64 = 1_000_000;
/// Neurons are metered in thousandths (Workers AI reports fractions, such
/// as 15.33) and priced per thousand neurons.
pub const MILLI_NEURONS_PER: u64 = 1_000_000;
/// Awake time is metered in milliseconds and priced per hour.
pub const AWAKE_MS_PER: u64 = 3_600_000;
/// Storage is metered in byte-hours (a sample's bytes × the hours since
/// the last sample) and priced per GB-month, taken as 10^9 bytes for 720
/// hours: Cloudflare's GB is no smaller and its month no shorter, so a
/// byte-hour here never costs less than Cloudflare's.
pub const STORAGE_BYTE_HOURS_PER: u64 = 1_000_000_000 * 720;
/// Requests are priced per million.
pub const REQUESTS_PER: u64 = 1_000_000;
/// Browser time is metered in milliseconds and priced per hour.
pub const BROWSER_MS_PER: u64 = 3_600_000;
/// Image transformations are priced per thousand.
pub const IMAGES_PER: u64 = 1_000;

/// What happened, in a meter's own unit (decision 24). The quantities are
/// exactly what the source counted; the book prices them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Usage {
    /// A model's tokens through AI Gateway, by its catalog id. `input`
    /// excludes cached tokens (OpenAI-shaped usage counts them in
    /// `prompt_tokens`, so the intercept subtracts them); `cache_write` is
    /// Anthropic's cache creation. Only the stream's last, cumulative
    /// usage is metered (spike S4).
    Tokens { model: String, input: u64, cached_input: u64, cache_write: u64, output: u64 },
    /// Workers AI neurons through the gateway, in thousandths of a neuron
    /// (the fraction Workers AI reports, × 1000, rounded up; an image's,
    /// from its tiles and steps: crate::media).
    Neurons { milli: u64 },
    /// A computer awake, in milliseconds, on its instance type (the
    /// book's name for it).
    Awake { instance: String, ms: u64 },
    /// Bytes held, sampled: the sample's bytes × the hours since the last.
    Storage { class: StorageClass, byte_hours: u64 },
    /// Requests served (each a router Worker request and the Durable
    /// Object request behind it).
    Requests { count: u64 },
    /// Unique dynamic workers (one fragment's code version) loaded on one
    /// UTC day.
    DynamicWorkers { count: u64 },
    /// Browser Rendering time, in milliseconds (a deploy's screenshot).
    Browser { ms: u64 },
    /// Cloudflare Images' unique transformations.
    Images { count: u64 },
    /// An operator key's use (decision 37), in the key's own unit: a call,
    /// unless its price counts something else.
    Key { key: String, units: u64 },
}

/// Where sampled bytes are held.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageClass {
    /// Blobs, site copies, screenshots and backups.
    R2,
    /// A fragment's SQLite (its Durable Object's).
    Sqlite,
    /// A fragment's git repository at code.storage.
    Git,
}

/// Why a usage is not one the book can price.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageFault {
    /// A model, instance or key name that is empty, too long, or not
    /// printable ASCII.
    Name,
    /// A quantity over `QUANTITY_MAX`.
    Quantity,
}

/// Whether `s` is 1 to `max` bytes of printable ASCII without spaces: the
/// shape of every id and name the ledger stores.
pub fn printable(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

impl Usage {
    /// Whether the usage is one a book can price: its names well formed,
    /// its quantities within `QUANTITY_MAX`.
    pub fn validate(&self) -> Result<(), UsageFault> {
        let name = match self {
            Usage::Tokens { model, .. } => Some(model),
            Usage::Awake { instance, .. } => Some(instance),
            Usage::Key { key, .. } => Some(key),
            _ => None,
        };
        if let Some(n) = name {
            if !printable(n, NAME_MAX_BYTES) {
                return Err(UsageFault::Name);
            }
        }
        let largest = match self {
            Usage::Tokens { input, cached_input, cache_write, output, .. } => *input.max(cached_input).max(cache_write).max(output),
            Usage::Neurons { milli } => *milli,
            Usage::Awake { ms, .. } | Usage::Browser { ms } => *ms,
            Usage::Storage { byte_hours, .. } => *byte_hours,
            Usage::Requests { count } | Usage::DynamicWorkers { count } | Usage::Images { count } => *count,
            Usage::Key { units, .. } => *units,
        };
        if largest <= QUANTITY_MAX {
            Ok(())
        } else {
            Err(UsageFault::Quantity)
        }
    }
}

/// A model's prices, micro-dollars per million tokens of each class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenPrices {
    pub input: i64,
    pub cached_input: i64,
    pub cache_write: i64,
    pub output: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPrice {
    /// The catalog id the intercept calls (`@cf/zai-org/glm-5.3-flash`).
    pub model: String,
    pub price: TokenPrices,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstancePrice {
    pub instance: String,
    /// Micro-dollars per awake hour.
    pub hour: i64,
}

/// Micro-dollars per GB-month (`STORAGE_BYTE_HOURS_PER`), by class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoragePrices {
    pub r2: i64,
    pub sqlite: i64,
    pub git: i64,
}

impl StoragePrices {
    fn of(&self, class: StorageClass) -> i64 {
        match class {
            StorageClass::R2 => self.r2,
            StorageClass::Sqlite => self.sqlite,
            StorageClass::Git => self.git,
        }
    }
}

/// An operator key's price: `micros` per `per` units (a search API at $5
/// per thousand calls is `5_000_000` per `1000`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyPrice {
    pub key: String,
    pub micros: i64,
    pub per: u64,
}

/// Every price, the fee and the margin, at one version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceBook {
    /// Grows with each change the operator makes: a ledger takes only a
    /// book newer than the one it has (crate::ledger, `set_book`).
    pub version: u32,
    pub margin_bp: u32,
    /// Applied to AI (tokens and neurons), which credits pay for.
    pub credits_fee_bp: u32,
    pub models: Vec<ModelPrice>,
    /// Micro-dollars per thousand neurons (`MILLI_NEURONS_PER`).
    pub neurons: i64,
    pub instances: Vec<InstancePrice>,
    pub storage: StoragePrices,
    /// Micro-dollars per million requests.
    pub requests: i64,
    /// Micro-dollars per unique dynamic worker per day.
    pub dynamic_workers: i64,
    /// Micro-dollars per browser hour.
    pub browser: i64,
    /// Micro-dollars per thousand unique image transformations.
    pub images: i64,
    pub keys: Vec<KeyPrice>,
}

// The default book, at list price on 2026-10-02, each with its source.

/// The tiers' models (decision 23). Sources: Workers AI's catalog
/// (`GET /ai/models/search`, `properties.price`) for both GLMs, and the AI
/// model catalog page for Opus 5.5, Anthropic's list price passed through
/// Unified Billing (spike S4; the gateway's own `cost` matched these on
/// every call). GLM has no cache writes: its write price is its input
/// price, so a write it reported would never be free. Clef (a job's
/// `ai.decide`: crate::decide) bills input tokens only (its catalog pages,
/// read 2026-10-07) and caches nothing: every input class is its input
/// price, its output free. A deployment whose ledgers already hold book 1
/// takes Clef's rows by naming a newer `price_book_version`.
///
/// The tiers' fallbacks when their model is busy (crate::models::ladder),
/// from their Workers AI catalog pages (read 2026-10-08): DeepSeek V4
/// Flash, and Gemma 4 26B A4B, which names no cached price (its cached
/// tokens are its input's). A deployment whose ledgers hold an older book
/// takes them by naming a newer `price_book_version`.
pub const DEFAULT_MODELS: [(&str, TokenPrices); 7] = [
    // $0.15 in, $0.03 cached, $0.50 out per million tokens
    ("@cf/zai-org/glm-5.3-flash", TokenPrices { input: 150_000, cached_input: 30_000, cache_write: 150_000, output: 500_000 }),
    // $1.40 in, $0.26 cached, $4.40 out
    ("@cf/zai-org/glm-5.3", TokenPrices { input: 1_400_000, cached_input: 260_000, cache_write: 1_400_000, output: 4_400_000 }),
    // $4 in, $0.20 cache read, $5 cache write, $20 out (the high tier, off for now)
    ("anthropic/claude-opus-5.5", TokenPrices { input: 4_000_000, cached_input: 200_000, cache_write: 5_000_000, output: 20_000_000 }),
    // $0.24 per million input tokens
    ("@cf/cloudflare/clef", TokenPrices { input: 240_000, cached_input: 240_000, cache_write: 240_000, output: 0 }),
    // $0.09 per million input tokens
    ("@cf/cloudflare/clef-flash", TokenPrices { input: 90_000, cached_input: 90_000, cache_write: 90_000, output: 0 }),
    // $0.44 in, $0.014 cached, $1.32 out
    ("@cf/deepseek-ai/deepseek-v4-flash-0731", TokenPrices { input: 440_000, cached_input: 14_000, cache_write: 440_000, output: 1_320_000 }),
    // $0.10 in, $0.30 out
    ("@cf/google/gemma-4-26b-a4b-it", TokenPrices { input: 100_000, cached_input: 100_000, cache_write: 100_000, output: 300_000 }),
];
/// Workers AI: $0.011 per thousand neurons (spike S4: neurons × $0.000011
/// matched tokens × the catalog price on every call). Images are priced
/// in neurons too (crate::media).
pub const DEFAULT_NEURONS: i64 = 11_000;
/// The default computer (decision 13: 2 vCPU, 6 GiB; a 12 GB disk
/// assumed), from Containers' pricing (Workers Paid, 2026-08-28):
/// - memory, provisioned: 6 GiB × $0.0000025 a GiB-second = $0.054 an hour;
/// - disk, provisioned: 12 GB × $0.00000007 a GB-second = $0.003024 an hour;
/// - CPU, billed on active use only, which the Computer DO cannot see:
///   5% of 2 vCPU (an idle guest) × $0.000020 a vCPU-second = $0.0072 an hour.
///
/// $0.064224 an awake hour, about $46 for an always-on month.
pub const DEFAULT_INSTANCES: [(&str, i64); 1] = [(INSTANCE, 64_224)];
/// The instance every computer runs on: its container starts at its size
/// (`instance_size`), and its awake time is priced by the book's row for it.
pub const INSTANCE: &str = "2vcpu-6gib";

/// The size a computer's container starts at (`ctx.container.start`'s
/// `instance`): one of Containers' named types, or a size of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum InstanceSize {
    Named(String),
    #[serde(rename_all = "camelCase")]
    Custom { vcpu: u32, memory_mib: u32, disk_mb: u32 },
}

/// Containers' named instance types (developers.cloudflare.com/containers/platform/limits).
const NAMED_INSTANCES: [&str; 6] = ["lite", "basic", "standard-1", "standard-2", "standard-3", "standard-4"];

/// The size an instance of the price book names: a named type as it is,
/// or `<n>vcpu-<m>gib` (the book's own names, decision 13's `2vcpu-6gib`)
/// as a custom size with a disk of 2 GB a GiB, as the price assumes, within
/// Containers' bounds for one: 1 to 4 vCPU, at most 12 GiB, at least 3 GiB
/// a vCPU, at most 20 GB of disk.
pub fn instance_size(name: &str) -> Result<InstanceSize, String> {
    if NAMED_INSTANCES.contains(&name) {
        return Ok(InstanceSize::Named(name.to_string()));
    }
    let custom = name.strip_suffix("gib").and_then(|n| n.split_once("vcpu-")).and_then(|(v, m)| Some((v.parse::<u32>().ok()?, m.parse::<u32>().ok()?)));
    let Some((vcpu, gib)) = custom else {
        return Err(format!("an instance is one of {} or <n>vcpu-<m>gib, not {name:?}", NAMED_INSTANCES.join(", ")));
    };
    if !(1..=4).contains(&vcpu) || gib > 12 || gib < 3 * vcpu {
        return Err(format!("{name}: a custom instance has 1 to 4 vCPU and at least 3 GiB a vCPU, at most 12 GiB"));
    }
    Ok(InstanceSize::Custom { vcpu, memory_mib: gib * 1024, disk_mb: (gib * 2_000).min(20_000) })
}
/// R2 Standard $0.015, Durable Objects' SQL storage $0.20 per GB-month.
/// code.storage publishes no price to us: git is at R2's until the
/// operator sets its own.
pub const DEFAULT_STORAGE: StoragePrices = StoragePrices { r2: 15_000, sqlite: 200_000, git: 15_000 };
/// Workers Standard $0.30 per million requests, plus Durable Objects'
/// $0.15 per million for the request each one makes of a fragment.
pub const DEFAULT_REQUESTS: i64 = 450_000;
/// Dynamic Workers: $0.002 per unique dynamic worker per day.
pub const DEFAULT_DYNAMIC_WORKERS: i64 = 2_000;
/// Browser Rendering (now "Browser Run"): $0.09 per browser hour on
/// Workers Paid, past the 10 hours a month the plan includes. Source:
/// developers.cloudflare.com/browser-run/pricing (last updated 2026-04-21,
/// read 2026-10-03); its time is totalled per day in seconds, and the
/// month's total rounded to the nearest hour. A card's shot is a Browser
/// Session (CDP over the binding: crate::card), which also bills
/// concurrent browsers, $2.00 each a month past 10 (the month's average of
/// each day's peak): a deployment-wide charge that no one shot causes, so
/// it is not metered per person (docs/ledger.md, Assumptions).
pub const DEFAULT_BROWSER: i64 = 90_000;
/// Cloudflare Images: $0.50 per thousand unique transformations.
pub const DEFAULT_IMAGES: i64 = 500_000;

/// The operator keys' list prices per call (decision 37; Paul, 2026-10-04:
/// Perplexity, Google Places, xAI, ElevenLabs), `(name, micros, per
/// calls)`, read from each vendor's own pricing page on 2026-10-03. A
/// catalog row of the operator kind takes its name's price here unless it
/// names its own (`fragment_core::catalog`). The swap meters a call, not
/// what the vendor counts (tokens, posts, minutes), so a vendor that bills
/// by those is priced at a typical call of the managed skill that uses it,
/// and the estimate says what it assumes.
pub const DEFAULT_KEYS: [(&str, i64, u64); 4] = [
    // Perplexity's Search API (`POST /search`, the perplexity-research
    // skill's search): $5.00 per 1,000
    // requests. Source: docs.perplexity.ai/getting-started/pricing. A Sonar
    // Pro brief (`/v1/sonar`) also bills tokens ($3 in, $15 out per
    // million) and a request fee of $6 to $14 per 1,000: it is metered at
    // the search's price, under what it costs (docs/technical-debt-ledger.md).
    ("perplexity", 5_000_000, 1_000),
    // Google Places API (New), Text Search Enterprise: $35.00 per 1,000
    // calls (0 to 100,000 a month). The goplaces skill's field masks name
    // rating, userRatingCount, websiteUri, nationalPhoneNumber, priceLevel
    // and regularOpeningHours, Enterprise fields; Place Details Enterprise
    // is $20.00, so a call is priced at the dearer of its two. Source:
    // developers.google.com/maps/billing-and-pricing/pricing (last updated
    // 2026-09-28), the free monthly caps not counted.
    ("google-places", 35_000_000, 1_000),
    // xAI's X Search (the x-search skill: `/v1/responses` with the
    // `x_search` tool): $5 per 1,000 posts fetched and $10 per 1,000 user
    // profiles, plus tokens. Source: docs.x.ai/developers/tools/x-search
    // (prices in effect since 2026-09-21) and docs.x.ai/docs/models
    // (grok-4.3: $1.25 in, $2.50 out per million). A call is taken as 20
    // posts ($0.10) and 10,000 tokens in and 1,000 out ($0.015): $0.115,
    // priced at $0.12.
    ("xai", 120_000, 1),
    // ElevenLabs' Music API (`/v1/music`, the music-generation skill):
    // $0.15 per minute of music generated. Source: elevenlabs.io/pricing/api.
    // A call is taken as a minute; the skill's jingles are 15 s, its
    // longest compositions 5 minutes. Text to speech is $0.08 per 1,000
    // characters on the same key.
    ("elevenlabs", 150_000, 1),
];

/// The list price of operator key `name` per call, when the price book
/// has one: `(micros, per calls)`.
pub fn default_key_price(name: &str) -> Option<(i64, u64)> {
    DEFAULT_KEYS.iter().find(|(n, _, _)| *n == name).map(|(_, micros, per)| (*micros, *per))
}

/// Why a book is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BookFault {
    /// A margin over `MARGIN_BP_MAX`, or the credits' fee over `FEE_BP_MAX`.
    Rate,
    /// A price below zero or over `PRICE_MAX`, or a key's `per` outside 1
    /// to `QUANTITY_MAX`.
    Price,
    /// A name that is not `printable`, or named twice in one list.
    Name,
    /// A list longer than `PRICES_MAX`.
    TooMany,
}

/// Why a usage has no charge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceError {
    Invalid(UsageFault),
    /// The book prices no such model, instance type or key.
    NoPrice,
    /// The charge is over `CHARGE_MAX`.
    TooLarge,
}

/// A usage's money, each figure rounded up once from the exact value:
/// Cloudflare's list price (what the gateway's log reports, for
/// reconciliation), the cost basis (list plus the credits fee), and the
/// charge (cost basis plus the margin).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Priced {
    pub list: i64,
    pub cost: i64,
    pub charge: i64,
}

impl PriceBook {
    /// The default book (version 1): the consts above, the default fee
    /// and margin, and no operator keys (decision 37 names services, not
    /// prices; the operator adds each key with its price).
    pub fn defaults() -> PriceBook {
        PriceBook {
            version: 1,
            margin_bp: MARGIN_BP_DEFAULT,
            credits_fee_bp: CREDITS_FEE_BP_DEFAULT,
            models: DEFAULT_MODELS.iter().map(|(model, price)| ModelPrice { model: model.to_string(), price: *price }).collect(),
            neurons: DEFAULT_NEURONS,
            instances: DEFAULT_INSTANCES.iter().map(|(instance, hour)| InstancePrice { instance: instance.to_string(), hour: *hour }).collect(),
            storage: DEFAULT_STORAGE,
            requests: DEFAULT_REQUESTS,
            dynamic_workers: DEFAULT_DYNAMIC_WORKERS,
            browser: DEFAULT_BROWSER,
            images: DEFAULT_IMAGES,
            keys: Vec::new(),
        }
    }

    /// Whether the book is one a ledger may charge with.
    pub fn validate(&self) -> Result<(), BookFault> {
        if self.margin_bp > MARGIN_BP_MAX || self.credits_fee_bp > FEE_BP_MAX {
            return Err(BookFault::Rate);
        }
        if self.models.len() > PRICES_MAX || self.instances.len() > PRICES_MAX || self.keys.len() > PRICES_MAX {
            return Err(BookFault::TooMany);
        }
        let fixed = [self.neurons, self.storage.r2, self.storage.sqlite, self.storage.git, self.requests, self.dynamic_workers, self.browser, self.images];
        let listed = self.models.iter().flat_map(|m| [m.price.input, m.price.cached_input, m.price.cache_write, m.price.output]);
        let listed = listed.chain(self.instances.iter().map(|i| i.hour)).chain(self.keys.iter().map(|k| k.micros));
        if !fixed.into_iter().chain(listed).all(|p| (0..=PRICE_MAX).contains(&p)) {
            return Err(BookFault::Price);
        }
        if !self.keys.iter().all(|k| (1..=QUANTITY_MAX).contains(&k.per)) {
            return Err(BookFault::Price);
        }
        let names_ok = |names: Vec<&str>| {
            let mut sorted = names.clone();
            sorted.sort_unstable();
            sorted.dedup();
            sorted.len() == names.len() && names.iter().all(|n| printable(n, NAME_MAX_BYTES))
        };
        let models = self.models.iter().map(|m| m.model.as_str()).collect();
        let instances = self.instances.iter().map(|i| i.instance.as_str()).collect();
        let keys = self.keys.iter().map(|k| k.key.as_str()).collect();
        if names_ok(models) && names_ok(instances) && names_ok(keys) {
            Ok(())
        } else {
            Err(BookFault::Name)
        }
    }

    /// What `usage` costs and is charged under this book.
    pub fn price(&self, usage: &Usage) -> Result<Priced, PriceError> {
        usage.validate().map_err(PriceError::Invalid)?;
        let ai = self.credits_fee_bp;
        let (numerator, per, fee_bp) = match usage {
            Usage::Tokens { model, input, cached_input, cache_write, output } => {
                let p = self.models.iter().find(|m| m.model == *model).ok_or(PriceError::NoPrice)?.price;
                let n = term(*input, p.input) + term(*cached_input, p.cached_input) + term(*cache_write, p.cache_write) + term(*output, p.output);
                (n, TOKENS_PER, ai)
            }
            Usage::Neurons { milli } => (term(*milli, self.neurons), MILLI_NEURONS_PER, ai),
            Usage::Awake { instance, ms } => {
                let hour = self.instances.iter().find(|i| i.instance == *instance).ok_or(PriceError::NoPrice)?.hour;
                (term(*ms, hour), AWAKE_MS_PER, 0)
            }
            Usage::Storage { class, byte_hours } => (term(*byte_hours, self.storage.of(*class)), STORAGE_BYTE_HOURS_PER, 0),
            Usage::Requests { count } => (term(*count, self.requests), REQUESTS_PER, 0),
            Usage::DynamicWorkers { count } => (term(*count, self.dynamic_workers), 1, 0),
            Usage::Browser { ms } => (term(*ms, self.browser), BROWSER_MS_PER, 0),
            Usage::Images { count } => (term(*count, self.images), IMAGES_PER, 0),
            Usage::Key { key, units } => {
                let k = self.keys.iter().find(|k| k.key == *key).ok_or(PriceError::NoPrice)?;
                (term(*units, k.micros), k.per, 0)
            }
        };
        priced(numerator, per, fee_bp, self.margin_bp)
    }
}

/// One part of a usage's exact list price, in micro-dollars × its unit's
/// count.
fn term(quantity: u64, micros: i64) -> i128 {
    assert!(quantity <= QUANTITY_MAX, "a usage is validated before it is priced");
    assert!((0..=PRICE_MAX).contains(&micros), "a book is validated before it is kept");
    i128::from(quantity) * i128::from(micros)
}

/// `numerator / per` micro-dollars at list, then with the fee, then with
/// the margin, each rounded up from the exact value (never from a figure
/// already rounded).
fn priced(numerator: i128, per: u64, fee_bp: u32, margin_bp: u32) -> Result<Priced, PriceError> {
    assert!(numerator >= 0, "prices and quantities are never negative");
    assert!(per >= 1, "a unit's count is at least one");
    assert!(fee_bp <= FEE_BP_MAX && margin_bp <= MARGIN_BP_MAX, "a book's rates are validated before it is kept");
    let (per, bp) = (i128::from(per), i128::from(BP));
    let with_fee = numerator * (bp + i128::from(fee_bp));
    let with_margin = with_fee * (bp + i128::from(margin_bp));
    assert!(with_margin <= NUMERATOR_MAX, "QUANTITY_MAX and PRICE_MAX keep the numerator in range");
    let charge = ceil_div(with_margin, per * bp * bp);
    if charge > i128::from(CHARGE_MAX) {
        return Err(PriceError::TooLarge);
    }
    let list = ceil_div(numerator, per);
    let cost = ceil_div(with_fee, per * bp);
    assert!(list <= cost && cost <= charge, "fees and margins only add");
    // each is at most `charge`, which fits CHARGE_MAX
    Ok(Priced { list: list as i64, cost: cost as i64, charge: charge as i64 })
}

fn ceil_div(n: i128, d: i128) -> i128 {
    assert!(n >= 0 && d > 0, "a ceiling of a non-negative amount");
    (n + d - 1) / d
}

/// Micro-dollars as dollars for people: `$0.0412`, `$20.00`.
pub fn dollars(m: i64) -> String {
    let sign = if m < 0 { "-" } else { "" };
    let m = m.unsigned_abs();
    let usd = USD as u64;
    let (whole, frac) = (m / usd, m % usd);
    if frac % 10_000 == 0 {
        format!("{sign}${whole}.{:02}", frac / 10_000)
    } else {
        format!("{sign}${whole}.{:06}", frac).trim_end_matches('0').to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The book's names start the size they price; named types pass as
    /// they are; a size Containers refuses is refused here first.
    #[test]
    fn instance_sizes() {
        assert_eq!(instance_size(INSTANCE), Ok(InstanceSize::Custom { vcpu: 2, memory_mib: 6144, disk_mb: 12_000 }));
        assert_eq!(serde_json::to_value(instance_size("2vcpu-6gib").unwrap()).unwrap(), serde_json::json!({ "vcpu": 2, "memoryMib": 6144, "diskMb": 12000 }));
        assert_eq!(instance_size("standard-3"), Ok(InstanceSize::Named("standard-3".into())));
        assert_eq!(serde_json::to_value(instance_size("standard-3").unwrap()).unwrap(), serde_json::json!("standard-3"));
        assert_eq!(instance_size("4vcpu-12gib"), Ok(InstanceSize::Custom { vcpu: 4, memory_mib: 12_288, disk_mb: 20_000 }));
        for bad in ["", "huge", "2vcpu-4gib", "5vcpu-15gib", "0vcpu-3gib", "1vcpu-13gib", "2vcpu-6", "vcpu-6gib", "-1vcpu-6gib"] {
            assert!(instance_size(bad).is_err(), "{bad:?}");
        }
    }

    fn tokens(model: &str, input: u64, cached_input: u64, cache_write: u64, output: u64) -> Usage {
        Usage::Tokens { model: model.into(), input, cached_input, cache_write, output }
    }

    const FLASH: &str = "@cf/zai-org/glm-5.3-flash";
    const GLM: &str = "@cf/zai-org/glm-5.3";
    const OPUS: &str = "anthropic/claude-opus-5.5";

    /// Goal: the default book is one a ledger accepts, and its version is
    /// the first. Method: validate it.
    #[test]
    fn the_defaults_are_a_valid_book() {
        let book = PriceBook::defaults();
        assert_eq!(book.validate(), Ok(()));
        assert_eq!(book.version, 1);
        assert_eq!((book.margin_bp, book.credits_fee_bp), (5_000, 500));
    }

    /// Goal: decision 24's formula on S3's simple turn. Method: a Flash
    /// turn that lists at exactly $0.004 (26,000 tokens in, 200 out)
    /// charges $0.0063 = 0.004 × 1.05 × 1.5, with a cost basis of $0.0042.
    #[test]
    fn a_four_tenths_of_a_cent_flash_turn_charges_0_0063() {
        let p = PriceBook::defaults().price(&tokens(FLASH, 26_000, 0, 0, 200)).unwrap();
        assert_eq!(p, Priced { list: 4_000, cost: 4_200, charge: 6_300 });
        assert_eq!(dollars(p.charge), "$0.0063");
    }

    /// Goal: our list price is the gateway's own `cost` (spike S4, B4).
    /// Method: the four calls whose gateway cost the spike recorded.
    #[test]
    fn list_prices_match_the_gateways_cost() {
        let book = PriceBook::defaults();
        let list = |u: Usage| book.price(&u).unwrap().list;
        assert_eq!(list(tokens(OPUS, 19, 0, 0, 5)), 176, "$0.000176");
        assert_eq!(list(tokens(OPUS, 411, 0, 0, 50)), 2_644, "$0.002644");
        assert_eq!(list(tokens(FLASH, 15, 0, 0, 3)), 4, "$0.00000375, rounded up");
        assert_eq!(list(tokens(GLM, 59, 128, 0, 12)), 169, "$0.00016868, rounded up");
    }

    /// Goal: neurons price a Workers AI call as its tokens do (spike S4:
    /// neurons × $0.000011 matched tokens × catalog price on every call).
    /// Method: the spike's metered calls (model, uncached in, cached in,
    /// out, neurons as Workers AI reported them): the exact token price and
    /// the neurons' agree within half a micro-dollar, and metering the
    /// neurons in thousandths (rounded up) charges within a micro-dollar
    /// of the tokens.
    #[test]
    fn neurons_and_tokens_agree_on_the_spikes_calls() {
        let calls: [(&str, u64, u64, u64, f64); 17] = [
            (FLASH, 187, 0, 12, 3.0954543352127075),
            (FLASH, 218, 0, 15, 3.654545336961746),
            (FLASH, 187, 0, 12, 3.095454216003418),
            (FLASH, 90, 128, 14, 2.2127268314361572),
            (GLM, 187, 0, 12, 28.599999278783798),
            (GLM, 218, 0, 11, 32.14545485377312),
            (GLM, 59, 128, 12, 15.334546089172363),
            (GLM, 218, 0, 14, 33.345455169677734),
            (FLASH, 187, 0, 30, 3.9136362075805664),
            (FLASH, 218, 0, 19, 3.8363635540008545),
            (GLM, 187, 0, 12, 28.599998474121094),
            (GLM, 218, 0, 11, 32.14545440673828),
            (FLASH, 6_055, 0, 3, 82.70453992486),
            (GLM, 50_460, 0, 3, 6_423.381640642881),
            (GLM, 122_016, 0, 3, 15_530.508593767881),
            (GLM, 257_908, 50_432, 3, 34_017.87968751788),
            (FLASH, 23, 0, 15, 0.9954546093940735),
        ];
        let book = PriceBook::defaults();
        for (model, input, cached, output, neurons) in calls {
            let p = book.models.iter().find(|m| m.model == model).unwrap().price;
            let exact_tokens = (input as f64 * p.input as f64 + cached as f64 * p.cached_input as f64 + output as f64 * p.output as f64) / 1e6;
            let exact_neurons = neurons * 11.0;
            assert!((exact_tokens - exact_neurons).abs() < 0.5, "{model} {input}/{cached}/{output}: {exact_tokens} µ$ by tokens, {exact_neurons} by neurons");
            let by_tokens = book.price(&tokens(model, input, cached, 0, output)).unwrap();
            let by_neurons = book.price(&Usage::Neurons { milli: (neurons * 1000.0).ceil() as u64 }).unwrap();
            assert!((by_tokens.charge - by_neurons.charge).abs() <= 1, "{model} {input}/{cached}/{output}: {by_tokens:?} {by_neurons:?}");
        }
    }

    /// Goal: Clef bills its input tokens only, at its catalog prices, with
    /// the credits' fee and the margin. Method: a million tokens in and a
    /// million out on each size.
    #[test]
    fn clef_bills_input_tokens_only() {
        let book = PriceBook::defaults();
        let million = |model: &str| book.price(&tokens(model, 1_000_000, 0, 0, 1_000_000)).unwrap();
        assert_eq!(million("@cf/cloudflare/clef"), Priced { list: 240_000, cost: 252_000, charge: 378_000 });
        assert_eq!(million("@cf/cloudflare/clef-flash"), Priced { list: 90_000, cost: 94_500, charge: 141_750 });
        assert_eq!(book.price(&tokens("@cf/cloudflare/clef-flash", 0, 1_000_000, 0, 0)).unwrap().list, 90_000, "nothing cached is cheaper");
    }

    /// Goal: a row rounds once, from its exact sum, never part by part.
    /// Method: one Flash token in and one cached lists at 0.18 µ$ and
    /// charges ceil(0.2835) = 1, where rounding each part would charge 2.
    #[test]
    fn a_row_rounds_up_once() {
        let book = PriceBook::defaults();
        assert_eq!(book.price(&tokens(FLASH, 1, 1, 0, 0)).unwrap(), Priced { list: 1, cost: 1, charge: 1 });
        assert_eq!(book.price(&tokens(FLASH, 0, 0, 0, 0)).unwrap(), Priced { list: 0, cost: 0, charge: 0 }, "nothing costs nothing");
        // 0.45 µ$ a request: rounded up, never down to free
        assert_eq!(book.price(&Usage::Requests { count: 1 }).unwrap(), Priced { list: 1, cost: 1, charge: 1 });
        assert_eq!(book.price(&Usage::Requests { count: 1_000_000 }).unwrap(), Priced { list: 450_000, cost: 450_000, charge: 675_000 });
    }

    /// Goal: only AI carries the credits fee; every other meter is its
    /// list price × 1.5. Method: one of each kind against the defaults.
    #[test]
    fn every_meter_kind_at_its_default_price() {
        let book = PriceBook::defaults();
        let price = |u: Usage| book.price(&u).unwrap();
        let hour = Usage::Awake { instance: "2vcpu-6gib".into(), ms: 3_600_000 };
        assert_eq!(price(hour), Priced { list: 64_224, cost: 64_224, charge: 96_336 });
        let month = Usage::Awake { instance: "2vcpu-6gib".into(), ms: 30 * 24 * 3_600_000 };
        assert_eq!(dollars(price(month).list), "$46.24128", "an always-on month at list");
        let gb_month = |class| Usage::Storage { class, byte_hours: 1_000_000_000 * 720 };
        assert_eq!(price(gb_month(StorageClass::Sqlite)), Priced { list: 200_000, cost: 200_000, charge: 300_000 });
        assert_eq!(price(gb_month(StorageClass::R2)), Priced { list: 15_000, cost: 15_000, charge: 22_500 });
        assert_eq!(price(gb_month(StorageClass::Git)).list, 15_000);
        assert_eq!(price(Usage::DynamicWorkers { count: 1 }), Priced { list: 2_000, cost: 2_000, charge: 3_000 });
        // a three-second screenshot: 75 µ$ at list
        assert_eq!(price(Usage::Browser { ms: 3_000 }), Priced { list: 75, cost: 75, charge: 113 });
        assert_eq!(price(Usage::Images { count: 1 }), Priced { list: 500, cost: 500, charge: 750 });
        // 1,000 neurons: $0.011 at list, × 1.05 × 1.5
        assert_eq!(price(Usage::Neurons { milli: 1_000_000 }), Priced { list: 11_000, cost: 11_550, charge: 17_325 });
        let mut keyed = book.clone();
        keyed.keys.push(KeyPrice { key: "search".into(), micros: 5_000_000, per: 1_000 });
        assert_eq!(keyed.price(&Usage::Key { key: "search".into(), units: 1 }).unwrap(), Priced { list: 5_000, cost: 5_000, charge: 7_500 }, "no credits fee on an operator key");
    }

    /// Goal: each operator key Paul named (2026-10-04) has a list price
    /// per call, and one call of each is charged that and the margin.
    #[test]
    fn the_operator_keys_list_prices() {
        let mut book = PriceBook::defaults();
        book.keys = DEFAULT_KEYS.iter().map(|(key, micros, per)| KeyPrice { key: key.to_string(), micros: *micros, per: *per }).collect();
        assert_eq!(book.validate(), Ok(()));
        let call = |key: &str| book.price(&Usage::Key { key: key.into(), units: 1 }).unwrap();
        assert_eq!(call("perplexity"), Priced { list: 5_000, cost: 5_000, charge: 7_500 }, "$5 per 1,000 searches");
        assert_eq!(call("google-places"), Priced { list: 35_000, cost: 35_000, charge: 52_500 }, "$35 per 1,000 Enterprise text searches");
        assert_eq!(call("xai"), Priced { list: 120_000, cost: 120_000, charge: 180_000 });
        assert_eq!(call("elevenlabs"), Priced { list: 150_000, cost: 150_000, charge: 225_000 }, "a minute of music");
        assert_eq!(default_key_price("perplexity"), Some((5_000_000, 1_000)));
        assert_eq!(default_key_price("nonesuch"), None);
    }

    /// Goal: the margin and the fee are the operator's. Method: a book at
    /// no margin charges its cost basis; at 100% twice it; at no fee the
    /// cost basis is the list.
    #[test]
    fn the_margin_and_fee_are_configurable() {
        let turn = tokens(FLASH, 26_000, 0, 0, 200);
        let mut book = PriceBook::defaults();
        book.margin_bp = 0;
        assert_eq!(book.price(&turn).unwrap(), Priced { list: 4_000, cost: 4_200, charge: 4_200 });
        book.margin_bp = 10_000;
        assert_eq!(book.price(&turn).unwrap().charge, 8_400);
        book.credits_fee_bp = 0;
        assert_eq!(book.price(&turn).unwrap(), Priced { list: 4_000, cost: 4_000, charge: 8_000 });
    }

    /// Goal: what the book cannot price is refused, typed, never charged
    /// at zero. Method: unknown names, bad names, quantities over the
    /// limit, and a charge over `CHARGE_MAX` at the largest quantities.
    #[test]
    fn what_cannot_be_priced_is_refused() {
        let book = PriceBook::defaults();
        assert_eq!(book.price(&tokens("@cf/other", 1, 0, 0, 1)), Err(PriceError::NoPrice));
        assert_eq!(book.price(&Usage::Awake { instance: "8vcpu".into(), ms: 1 }), Err(PriceError::NoPrice));
        assert_eq!(book.price(&Usage::Key { key: "search".into(), units: 1 }), Err(PriceError::NoPrice));
        assert_eq!(book.price(&tokens("", 1, 0, 0, 1)), Err(PriceError::Invalid(UsageFault::Name)));
        assert_eq!(book.price(&tokens("a model", 1, 0, 0, 1)), Err(PriceError::Invalid(UsageFault::Name)));
        assert_eq!(book.price(&tokens(&"m".repeat(NAME_MAX_BYTES + 1), 1, 0, 0, 1)), Err(PriceError::Invalid(UsageFault::Name)));
        assert_eq!(book.price(&tokens(FLASH, QUANTITY_MAX + 1, 0, 0, 0)), Err(PriceError::Invalid(UsageFault::Quantity)));
        assert_eq!(book.price(&Usage::Requests { count: u64::MAX }), Err(PriceError::Invalid(UsageFault::Quantity)));
        // the largest quantities at the largest price: refused, no overflow
        let mut dear = PriceBook::defaults();
        dear.margin_bp = MARGIN_BP_MAX;
        dear.credits_fee_bp = FEE_BP_MAX;
        dear.models[0].price = TokenPrices { input: PRICE_MAX, cached_input: PRICE_MAX, cache_write: PRICE_MAX, output: PRICE_MAX };
        let q = QUANTITY_MAX;
        assert_eq!(dear.price(&tokens(FLASH, q, q, q, q)), Err(PriceError::TooLarge));
        assert_eq!(dear.price(&Usage::DynamicWorkers { count: q }), Err(PriceError::TooLarge));
    }

    /// Goal: a book that breaks a rule is refused with the rule. Method:
    /// one fault of each kind on the defaults.
    #[test]
    fn a_bad_book_is_refused() {
        let fault = |f: fn(&mut PriceBook)| {
            let mut b = PriceBook::defaults();
            f(&mut b);
            b.validate()
        };
        assert_eq!(fault(|b| b.margin_bp = MARGIN_BP_MAX + 1), Err(BookFault::Rate));
        assert_eq!(fault(|b| b.credits_fee_bp = FEE_BP_MAX + 1), Err(BookFault::Rate));
        assert_eq!(fault(|b| b.requests = -1), Err(BookFault::Price));
        assert_eq!(fault(|b| b.models[1].price.output = PRICE_MAX + 1), Err(BookFault::Price));
        assert_eq!(fault(|b| b.keys.push(KeyPrice { key: "k".into(), micros: 1, per: 0 })), Err(BookFault::Price));
        assert_eq!(fault(|b| b.models[1].model = FLASH.into()), Err(BookFault::Name), "a model priced twice");
        assert_eq!(fault(|b| b.instances[0].instance = String::new()), Err(BookFault::Name));
        assert_eq!(
            fault(|b| b.keys = (0..=PRICES_MAX).map(|i| KeyPrice { key: format!("k{i}"), micros: 1, per: 1 }).collect()),
            Err(BookFault::TooMany)
        );
        assert_eq!(fault(|b| b.keys.push(KeyPrice { key: "k".into(), micros: 1, per: 1 })), Ok(()));
    }

    #[test]
    fn money_for_people() {
        assert_eq!(dollars(20 * USD), "$20.00");
        assert_eq!(dollars(40_000), "$0.04");
        assert_eq!(dollars(41_234), "$0.041234");
        assert_eq!(dollars(-2 * USD), "-$2.00");
        assert_eq!(dollars(i64::MIN), "-$9223372036854.775808");
    }

    /// Usages are tagged by kind, in snake_case, and refuse unknown fields.
    #[test]
    fn a_usage_is_tagged_by_kind() {
        let u = tokens(FLASH, 1, 2, 3, 4);
        assert_eq!(
            serde_json::to_value(&u).unwrap(),
            serde_json::json!({ "kind": "tokens", "model": FLASH, "input": 1, "cached_input": 2, "cache_write": 3, "output": 4 })
        );
        let s = Usage::Storage { class: StorageClass::Sqlite, byte_hours: 9 };
        assert_eq!(serde_json::to_value(&s).unwrap(), serde_json::json!({ "kind": "storage", "class": "sqlite", "byte_hours": 9 }));
        assert!(serde_json::from_value::<Usage>(serde_json::json!({ "kind": "requests", "count": 1, "extra": 2 })).is_err());
        assert!(serde_json::from_value::<Usage>(serde_json::json!({ "kind": "requests", "count": -1 })).is_err(), "quantities are unsigned");
        let billed = serde_json::json!({ "kind": "billed", "vendor": "openrouter", "micros": 1 });
        assert!(serde_json::from_value::<Usage>(billed).is_err(), "a vendor's own bill went with OpenRouter: every meter is Cloudflare's");
    }
}
