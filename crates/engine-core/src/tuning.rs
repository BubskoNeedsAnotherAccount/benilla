//! Bolt-on modifications: intake, exhaust and forced induction.
//!
//! The base `EngineConfig` describes the stock engine. A [`Tuning`] sits on top of it and bends the
//! same model the stock engine uses (volumetric-efficiency curve, exhaust backpressure, boost and
//! charge temperature), so every part you swap changes the sim for a physical reason. A default
//! `Tuning` is exactly the stock engine - the regression dyno figures do not move.

use crate::acoustics::PipeMaterial;
use crate::config::{EngineConfig, TurboConfig};

// ---------------------------------------------------------------- intake

/// Air filter / inlet tract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AirFilter {
    /// Paper element in a sealed airbox.
    #[default]
    Stock,
    /// Open cone filter on a smooth tube fed from outside the bay: less restriction, cooler air.
    ColdAir,
    /// Naked velocity stack: least restriction, but it ingests hot underhood air.
    OpenStack,
}

impl AirFilter {
    pub const ALL: [AirFilter; 3] = [AirFilter::Stock, AirFilter::ColdAir, AirFilter::OpenStack];
    pub fn label(self) -> &'static str {
        match self {
            AirFilter::Stock => "Stock airbox",
            AirFilter::ColdAir => "Cold-air cone",
            AirFilter::OpenStack => "Open velocity stack",
        }
    }
    /// Extra volumetric efficiency at redline (it scales with airflow, so with rpm squared).
    fn flow_gain(self) -> f64 {
        match self {
            AirFilter::Stock => 0.0,
            AirFilter::ColdAir => 0.012,
            AirFilter::OpenStack => 0.02,
        }
    }
    /// Change in charge temperature, kelvin.
    fn temp_delta_k(self) -> f64 {
        match self {
            AirFilter::Stock => 0.0,
            AirFilter::ColdAir => -7.0,
            AirFilter::OpenStack => 4.0,
        }
    }
}

/// What happens to the charge pressure when the throttle closes on a turbo engine. Only the sound differs (the dyno does not
/// move): a diverter valve recirculates it quietly, a vent valve dumps it to the air with a loud hiss, and with nothing the
/// compressor surges against the closed throttle: the flutter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BlowOff {
    /// Recirculating diverter valve, as on production cars: a short, soft hiss.
    #[default]
    Recirculating,
    /// Atmospheric blow-off valve: the loud "pssh".
    Vented,
    /// No valve: compressor surge, "fut-fut-fut".
    None,
}

impl BlowOff {
    pub const ALL: [BlowOff; 3] = [BlowOff::Recirculating, BlowOff::Vented, BlowOff::None];
    pub fn label(self) -> &'static str {
        match self {
            BlowOff::Recirculating => "Diverter valve (stock)",
            BlowOff::Vented => "Vented blow-off valve",
            BlowOff::None => "None (compressor flutter)",
        }
    }
}

/// Intake manifold runner / plenum tuning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Runners {
    /// Long, narrow runners: torque down low, runs out of breath early.
    LongRunner,
    #[default]
    Stock,
    /// Short, fat runners and a big plenum: top-end power, softer bottom end.
    ShortRunner,
    /// One throttle per cylinder: sharp response and the highest ceiling, gutless at idle.
    Itb,
}

impl Runners {
    pub const ALL: [Runners; 4] = [Runners::LongRunner, Runners::Stock, Runners::ShortRunner, Runners::Itb];
    pub fn label(self) -> &'static str {
        match self {
            Runners::LongRunner => "Long runners (torque)",
            Runners::Stock => "Stock manifold",
            Runners::ShortRunner => "Short runners (power)",
            Runners::Itb => "Individual throttle bodies",
        }
    }
    /// Multiplier on the rpm where volumetric efficiency peaks.
    fn peak_rpm_scale(self) -> f64 {
        match self {
            Runners::LongRunner => 0.80,
            Runners::Stock => 1.0,
            Runners::ShortRunner => 1.14,
            Runners::Itb => 1.22,
        }
    }
    /// Width of the VE hump.
    fn spread_scale(self) -> f64 {
        match self {
            Runners::LongRunner => 0.9,
            Runners::Stock => 1.0,
            Runners::ShortRunner => 1.05,
            Runners::Itb => 1.1,
        }
    }
    fn base_delta(self) -> f64 {
        match self {
            Runners::LongRunner => 0.03,
            Runners::Stock => 0.0,
            Runners::ShortRunner => -0.025,
            Runners::Itb => -0.05,
        }
    }
    fn bonus_scale(self) -> f64 {
        match self {
            Runners::LongRunner => 1.02,
            Runners::Stock => 1.0,
            Runners::ShortRunner => 1.07,
            Runners::Itb => 1.15,
        }
    }
}

// ---------------------------------------------------------------- exhaust

/// Exhaust manifold / header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Header {
    /// Short cast-iron log manifold.
    #[default]
    Stock,
    /// Short tubular headers.
    Shorty,
    /// Long equal-length tubes: good scavenging, big mid and top-end.
    LongTube,
}

impl Header {
    pub const ALL: [Header; 3] = [Header::Stock, Header::Shorty, Header::LongTube];
    pub fn label(self) -> &'static str {
        match self {
            Header::Stock => "Stock manifold",
            Header::Shorty => "Shorty headers",
            Header::LongTube => "Long-tube headers",
        }
    }
    fn flow_mult(self) -> f64 {
        match self {
            Header::Stock => 1.0,
            Header::Shorty => 0.85,
            Header::LongTube => 0.72,
        }
    }
    /// What this style of header is normally made of.
    pub fn natural_material(self) -> PipeMaterial {
        match self {
            Header::Stock => PipeMaterial::CastIron,
            Header::Shorty => PipeMaterial::Stainless409,
            Header::LongTube => PipeMaterial::Stainless304,
        }
    }
    /// Scavenging: pulse tuning that pulls the next charge in.
    fn scavenge(self) -> f64 {
        match self {
            Header::Stock => 0.0,
            Header::Shorty => 0.008,
            Header::LongTube => 0.02,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Cat {
    /// Ceramic monolith.
    #[default]
    Stock,
    /// Metallic substrate, lower cell density.
    HighFlow,
    /// Test pipe. Free flow, loud, illegal.
    Delete,
}

impl Cat {
    pub const ALL: [Cat; 3] = [Cat::Stock, Cat::HighFlow, Cat::Delete];
    pub fn label(self) -> &'static str {
        match self {
            Cat::Stock => "Stock catalyst",
            Cat::HighFlow => "High-flow cat",
            Cat::Delete => "Cat delete (test pipe)",
        }
    }
    fn flow_mult(self) -> f64 {
        match self {
            Cat::Stock => 1.0,
            Cat::HighFlow => 0.7,
            Cat::Delete => 0.38,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Muffler {
    #[default]
    Stock,
    /// Chambered performance muffler.
    Sport,
    /// Straight-through "race" can.
    Straight,
    /// No muffler at all.
    Delete,
}

impl Muffler {
    pub const ALL: [Muffler; 4] = [Muffler::Stock, Muffler::Sport, Muffler::Straight, Muffler::Delete];
    pub fn label(self) -> &'static str {
        match self {
            Muffler::Stock => "Stock muffler",
            Muffler::Sport => "Sport muffler",
            Muffler::Straight => "Straight-through can",
            Muffler::Delete => "Muffler delete",
        }
    }
    fn flow_mult(self) -> f64 {
        match self {
            Muffler::Stock => 1.0,
            Muffler::Sport => 0.78,
            Muffler::Straight => 0.55,
            Muffler::Delete => 0.4,
        }
    }
    /// 0 = silent, 1 = bottled up. Drives the exhaust low-pass in the audio synth.
    pub fn damping(self) -> f64 {
        match self {
            Muffler::Stock => 0.5,
            Muffler::Sport => 0.32,
            Muffler::Straight => 0.16,
            Muffler::Delete => 0.0,
        }
    }
}

/// How the two sides of a dual exhaust are tied together.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Crossover {
    /// Two independent pipes.
    None,
    /// A balance tube between the pipes: smooths the note, lowers the pitch.
    #[default]
    HPipe,
    /// The pipes merge and split again: higher, smoother, rasp-free "sewing machine" note.
    XPipe,
}

impl Crossover {
    pub const ALL: [Crossover; 3] = [Crossover::None, Crossover::HPipe, Crossover::XPipe];
    pub fn label(self) -> &'static str {
        match self {
            Crossover::None => "None (independent pipes)",
            Crossover::HPipe => "H-pipe",
            Crossover::XPipe => "X-pipe",
        }
    }
}

// ---------------------------------------------------------------- forced induction

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Intercooler {
    None,
    #[default]
    Stock,
    /// Big front-mount core.
    Large,
}

impl Intercooler {
    pub const ALL: [Intercooler; 3] = [Intercooler::None, Intercooler::Stock, Intercooler::Large];
    pub fn label(self) -> &'static str {
        match self {
            Intercooler::None => "None",
            Intercooler::Stock => "Stock intercooler",
            Intercooler::Large => "Large front-mount",
        }
    }
    /// Fraction of the compressor's heat rise the core removes.
    pub fn effectiveness(self) -> f64 {
        match self {
            Intercooler::None => 0.0,
            Intercooler::Stock => STOCK_INTERCOOLER_EFF,
            Intercooler::Large => 0.88,
        }
    }
}

/// The effectiveness the stock sim was calibrated at (charge temp = ambient).
pub const STOCK_INTERCOOLER_EFF: f64 = 0.7;
/// Compressor discharge rise per bar of boost (kelvin).
const COMPRESSOR_RISE_K_PER_BAR: f64 = 80.0;
/// Fewest and most turbos the rig can carry.
pub const MAX_TURBOS: u8 = 4;
/// Boost slider range (bar gauge).
pub const BOOST_MIN_BAR: f64 = 0.2;
pub const BOOST_MAX_BAR: f64 = 2.2;

/// A complete set of bolt-ons. `Default` is the stock engine.
#[derive(Clone, Debug, PartialEq)]
pub struct Tuning {
    pub air_filter: AirFilter,
    pub runners: Runners,
    pub header: Header,
    pub cat: Cat,
    pub muffler: Muffler,
    /// Exhaust pipe diameter relative to stock (0.85..1.35).
    pub pipe_scale: f64,
    /// Number of turbochargers. `None` = whatever the stock engine ships with.
    pub turbo_count: Option<u8>,
    /// Per-turbo size (0.6 small/quick .. 1.6 big/laggy). 1.0 = stock.
    pub turbo_size: f64,
    /// Wastegate target in bar gauge. `None` = the stock boost.
    pub boost_bar: Option<f64>,
    pub intercooler: Intercooler,
    /// What the charge pressure does on a throttle lift (sound only).
    pub blow_off: BlowOff,
    // ---- exhaust acoustics: what the pipes are made of and how long / wide they are
    /// Header material. `None` = what that header style is normally made of.
    pub header_material: Option<PipeMaterial>,
    /// Material of the cat, pipes and muffler shell.
    pub pipe_material: PipeMaterial,
    /// Wall thickness relative to the material's usual gauge (0.5..2).
    pub wall_scale: f64,
    /// Heat wrap on the header primaries.
    pub header_wrap: bool,
    /// Primary tube length / diameter relative to the header style (0.6..1.6 / 0.8..1.3).
    pub primary_len_scale: f64,
    pub primary_dia_scale: f64,
    /// Mid-pipe and tailpipe length relative to stock.
    pub mid_len_scale: f64,
    pub tail_len_scale: f64,
    pub crossover: Crossover,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            air_filter: AirFilter::Stock,
            runners: Runners::Stock,
            header: Header::Stock,
            cat: Cat::Stock,
            muffler: Muffler::Stock,
            pipe_scale: 1.0,
            turbo_count: None,
            turbo_size: 1.0,
            boost_bar: None,
            intercooler: Intercooler::Stock,
            blow_off: BlowOff::Recirculating,
            header_material: None,
            pipe_material: PipeMaterial::MildSteel,
            wall_scale: 1.0,
            header_wrap: false,
            primary_len_scale: 1.0,
            primary_dia_scale: 1.0,
            mid_len_scale: 1.0,
            tail_len_scale: 1.0,
            crossover: Crossover::HPipe,
        }
    }
}

/// How many turbos the stock engine ships with (the figure the turbo config describes).
pub fn stock_turbo_count(cfg: &EngineConfig) -> u8 {
    u8::from(cfg.turbo.is_some())
}

/// The turbo setup the sim actually runs, resolved against the stock engine.
#[derive(Clone, Copy, Debug)]
pub struct TurboSetup {
    pub count: u8,
    pub max_boost_pa: f64,
    pub full_spool_rpm: f64,
    pub spool_tau_s: f64,
    /// rpm above which the compressors run out of flow and boost starts to fall away.
    pub choke_rpm: f64,
}

impl Tuning {
    pub fn is_stock(&self) -> bool {
        *self == Tuning::default()
    }

    pub fn turbo_count_for(&self, cfg: &EngineConfig) -> u8 {
        self.turbo_count.unwrap_or_else(|| stock_turbo_count(cfg)).min(MAX_TURBOS)
    }

    pub fn boost_bar_for(&self, cfg: &EngineConfig) -> f64 {
        self.boost_bar
            .unwrap_or_else(|| cfg.turbo.as_ref().map(|t| t.max_boost_pa / 1.0e5).unwrap_or(0.7))
            .clamp(BOOST_MIN_BAR, BOOST_MAX_BAR)
    }

    /// Resolves the turbo hardware for this engine, or `None` if it is naturally aspirated.
    pub fn turbo_setup(&self, cfg: &EngineConfig) -> Option<TurboSetup> {
        let count = self.turbo_count_for(cfg);
        if count == 0 {
            return None;
        }
        let stock_count = stock_turbo_count(cfg);
        let size = self.turbo_size.clamp(0.6, 1.6);
        // Stock hardware is the reference; an NA engine gets a plausible turbo derived from its
        // redline and displacement.
        let base = cfg.turbo.clone().unwrap_or(TurboConfig {
            max_boost_pa: 0.7e5,
            full_spool_rpm: cfg.redline_rpm * 0.42,
            spool_time_constant_s: 0.30,
        });
        let ref_count = stock_count.max(1) as f64;
        let count_ratio = count as f64 / ref_count;

        // Several smaller turbines come up on song sooner and respond quicker; a big one lags.
        let full_spool_rpm = base.full_spool_rpm * size / count_ratio.powf(0.3);
        let spool_tau_s = base.spool_time_constant_s * size.powf(1.2) / count_ratio.sqrt();

        // Flow capacity vs demand. The stock layout is sized so it never chokes; a naturally
        // aspirated engine's single unit covers ~62% of what it would need at redline.
        let boost_bar = self.boost_bar_for(cfg);
        let stock_boost_bar = cfg.turbo.as_ref().map(|t| t.max_boost_pa / 1.0e5).unwrap_or(0.7);
        let stock_capacity = if cfg.turbo.is_some() { ref_count } else { 1.6 };
        let capacity = count as f64 * size;
        let pressure_demand = (1.0 + boost_bar) / (1.0 + stock_boost_bar);
        let ratio = capacity / stock_capacity / pressure_demand;
        let choke_rpm = cfg.redline_rpm * 1.05 * ratio.min(1.0e3);

        Some(TurboSetup { count, max_boost_pa: boost_bar * 1.0e5, full_spool_rpm, spool_tau_s, choke_rpm })
    }

    /// Fraction of target boost still delivered at `rpm` once the compressors start to choke.
    pub fn flow_droop(setup: &TurboSetup, rpm: f64) -> f64 {
        if rpm <= setup.choke_rpm {
            1.0
        } else {
            (setup.choke_rpm / rpm).powf(1.6)
        }
    }

    /// Extra heat the charge picks up (negative = cooler) compared with the stock calibration.
    pub fn charge_temp_delta_k(&self, boost_pa: f64) -> f64 {
        let rise = COMPRESSOR_RISE_K_PER_BAR * (boost_pa / 1.0e5).max(0.0);
        let ic = (STOCK_INTERCOOLER_EFF - self.intercooler.effectiveness()) * rise;
        (ic + self.air_filter.temp_delta_k()).clamp(-30.0, 120.0)
    }

    // ---- volumetric efficiency

    pub fn ve_peak_rpm(&self, base: f64) -> f64 {
        base * self.runners.peak_rpm_scale()
    }
    pub fn ve_spread_rpm(&self, base: f64) -> f64 {
        base * self.runners.spread_scale()
    }
    pub fn ve_base(&self, base: f64) -> f64 {
        base + self.runners.base_delta()
    }
    pub fn ve_peak_bonus(&self, base: f64) -> f64 {
        base * self.runners.bonus_scale()
    }
    /// Multiplier from the inlet tract and header scavenging at `rpm`.
    pub fn ve_multiplier(&self, cfg: &EngineConfig, rpm: f64) -> f64 {
        let r = (rpm / cfg.redline_rpm).clamp(0.0, 1.2);
        let flow = self.air_filter.flow_gain() * r * r;
        let x = (rpm - 0.8 * self.ve_peak_rpm(cfg.ve_peak_rpm)) / (1.2 * cfg.ve_spread_rpm);
        let scav = self.header.scavenge() * (-x * x).exp() * self.scavenge_tuning(cfg, rpm);
        1.0 + flow + scav
    }

    // ---- exhaust

    /// Backpressure multiplier contributed by the bolt-ons (1.0 = stock).
    pub fn exhaust_flow_multiplier(&self) -> f64 {
        let pipe = self.pipe_scale.clamp(0.7, 1.5);
        let pipe_mult = 1.0 / pipe.powf(2.4);
        // longer pipes rub more gas (friction ~ length / diameter^5); fatter primaries flow better.
        // All of these are exactly 1 at their stock values.
        let primary = self.primary_len_scale.clamp(0.5, 2.0).powf(0.35) / self.primary_dia_scale.clamp(0.7, 1.5).powf(1.8);
        let run = (0.6 * self.mid_len_scale.clamp(0.3, 2.5) + 0.4 * self.tail_len_scale.clamp(0.3, 2.5)).powf(0.25);
        self.header.flow_mult() * self.cat.flow_mult() * self.muffler.flow_mult() * pipe_mult * primary * run
    }

    /// Length of the port (valve to header flange), m.
    pub fn port_length(cfg: &EngineConfig) -> f64 {
        0.040 + 0.40 * cfg.bore_meters
    }

    /// Exhaust duration in the header formula: 180 deg plus the crank angle the valve opens before BDC.
    pub fn exhaust_duration_deg(cfg: &EngineConfig) -> f64 {
        180.0 + (540.0 - crate::acoustics::valve_train(cfg).exhaust.open as f64)
    }

    /// The rpm whose primary tube of total length `len_m` (valve to collector) the classic header
    /// formula `L[in] = 850 ED / rpm - 3` tunes. Dyno-derived, and the solver reproduces it: the
    /// pressure at the valve during overlap bottoms out near this rpm, with a second dip near 0.6x.
    pub fn tuned_rpm(cfg: &EngineConfig, len_m: f64) -> f64 {
        850.0 * Self::exhaust_duration_deg(cfg) / (len_m / 0.0254 + 3.0)
    }

    /// Primary length (excluding the port) of the stock tube of this header style.
    pub fn header_base_length(&self, cfg: &EngineConfig) -> f64 {
        let bore = cfg.bore_meters;
        match self.header {
            Header::Stock => 0.11 + 0.45 * bore,
            Header::Shorty => 0.18 + 1.6 * bore,
            // long tubes are tuned for the power peak (about 1.22x the torque peak)
            Header::LongTube => {
                let rpm = 1.22 * cfg.ve_peak_rpm.max(1500.0);
                let total = 0.0254 * (850.0 * Self::exhaust_duration_deg(cfg) / rpm - 3.0);
                (total - Self::port_length(cfg)).clamp(0.35, 1.15)
            }
        }
    }

    /// Valve-to-collector length of the primaries as fitted, m.
    pub fn primary_total_length(&self, cfg: &EngineConfig) -> f64 {
        Self::port_length(cfg) + self.header_base_length(cfg) * self.primary_len_scale.clamp(0.5, 2.0)
    }

    /// Scavenging multiplier (1.0 at stock tube geometry) from how well the primary tubes are tuned to
    /// `rpm` compared with the stock tubes: tuning peaks at the formula's rpm and, weaker, at 0.6 of it.
    pub fn scavenge_tuning(&self, cfg: &EngineConfig, rpm: f64) -> f64 {
        if self.primary_len_scale == 1.0 && self.primary_dia_scale == 1.0 {
            return 1.0;
        }
        let stock_len = Self::port_length(cfg) + self.header_base_length(cfg);
        let bump = |len: f64| {
            let t = Self::tuned_rpm(cfg, len);
            let g = |c: f64| {
                let x = (rpm.max(100.0) / c).ln() / 0.20;
                (-x * x).exp()
            };
            0.35 + g(t) + 0.6 * g(0.6 * t)
        };
        // fatter tubes carry the pulse with less loss, thin ones choke the scavenging
        let dia = self.primary_dia_scale.clamp(0.7, 1.5).powf(0.6);
        (bump(self.primary_total_length(cfg)) / bump(stock_len) * dia).clamp(0.3, 2.0)
    }

    /// Catalyst is gone: no light-off, tailpipe pops get louder.
    pub fn is_cat_deleted(&self) -> bool {
        self.cat == Cat::Delete
    }
}

impl Tuning {
    /// Parses a comma-separated parts list, e.g.
    /// `turbos=2,size=0.9,boost=1.0,filter=cold|open,runners=long|short|itb,header=shorty|long,cat=highflow|delete,`
    /// `muffler=sport|straight|delete,pipe=1.2,ic=none|large,bov=stock|vent|none,pipemat=ti|ss304|ss409|steel|inconel,`
    /// `headermat=ti|ss304|ss409|steel|inconel|iron,wall=1.3,wrap=1,plen=0.8,pdia=1.1,mid=1.3,tail=1.5,cross=none|h|x`.
    pub fn parse_spec(spec: &str) -> Result<Tuning, String> {
        let mut t = Tuning::default();
        let mat = |v: &str| -> Result<PipeMaterial, String> {
            Ok(match v {
                "steel" | "mild" => PipeMaterial::MildSteel,
                "ss409" | "409" => PipeMaterial::Stainless409,
                "ss304" | "304" | "ss" => PipeMaterial::Stainless304,
                "ti" | "titanium" => PipeMaterial::Titanium,
                "inconel" => PipeMaterial::Inconel,
                "iron" | "cast" => PipeMaterial::CastIron,
                _ => return Err(format!("unknown material {v}")),
            })
        };
        for kv in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (k, v) = kv.split_once('=').ok_or_else(|| format!("expected key=value, got {kv}"))?;
            let f = v.parse::<f64>().ok();
            match (k, v) {
                ("turbos", _) => t.turbo_count = v.parse().ok(),
                ("size", _) => t.turbo_size = f.unwrap_or(1.0),
                ("boost", _) => t.boost_bar = f,
                ("pipe", _) => t.pipe_scale = f.unwrap_or(1.0),
                ("filter", "cold") => t.air_filter = AirFilter::ColdAir,
                ("filter", "open") => t.air_filter = AirFilter::OpenStack,
                ("runners", "long") => t.runners = Runners::LongRunner,
                ("runners", "short") => t.runners = Runners::ShortRunner,
                ("runners", "itb") => t.runners = Runners::Itb,
                ("header", "shorty") => t.header = Header::Shorty,
                ("header", "long") => t.header = Header::LongTube,
                ("cat", "highflow") => t.cat = Cat::HighFlow,
                ("cat", "delete") => t.cat = Cat::Delete,
                ("muffler", "sport") => t.muffler = Muffler::Sport,
                ("muffler", "straight") => t.muffler = Muffler::Straight,
                ("muffler", "delete") => t.muffler = Muffler::Delete,
                ("ic", "none") => t.intercooler = Intercooler::None,
                ("ic", "large") => t.intercooler = Intercooler::Large,
                ("bov", "stock") | ("bov", "diverter") => t.blow_off = BlowOff::Recirculating,
                ("bov", "vent") => t.blow_off = BlowOff::Vented,
                ("bov", "none") => t.blow_off = BlowOff::None,
                ("pipemat", m) => t.pipe_material = mat(m)?,
                ("headermat", m) => t.header_material = Some(mat(m)?),
                ("wall", _) => t.wall_scale = f.unwrap_or(1.0),
                ("wrap", _) => t.header_wrap = v == "1" || v == "true",
                ("plen", _) => t.primary_len_scale = f.unwrap_or(1.0),
                ("pdia", _) => t.primary_dia_scale = f.unwrap_or(1.0),
                ("mid", _) => t.mid_len_scale = f.unwrap_or(1.0),
                ("tail", _) => t.tail_len_scale = f.unwrap_or(1.0),
                ("cross", "none") => t.crossover = Crossover::None,
                ("cross", "h") => t.crossover = Crossover::HPipe,
                ("cross", "x") => t.crossover = Crossover::XPipe,
                _ => return Err(format!("unknown tuning entry {kv}")),
            }
        }
        Ok(t)
    }
}
