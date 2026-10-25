//! Full Continent Asset Extractor for benilla-lambo Web / WASM deployment.
//!
//! Fully extracts Map ID 0 (Eastern Kingdoms / Azeroth) and Map ID 1 (Kalimdor)
//! from the WoW 1.12.1 MPQ patch chain, including:
//! - All ADT terrain tiles for both continents
//! - Continent WDT and WDL headers
//! - Base DBC databases required by the world engine
//! - All WMO buildings, structures, and their group files
//! - All M2 doodads (trees, props, signs, objects)
//! - All referenced BLP terrain and model textures
//!
//! Features resumable extraction: already extracted files are skipped unless `--force` is given.

use std::collections::HashSet;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::Parser;

use benilla_adt::{parse_adt, ParsedAdt};
use benilla_formats::Chain;
use benilla_m2::parse_m2;
use benilla_wmo::{parse_wmo, ParsedWmo};

/// Extraction CLI options.
#[derive(Parser, Debug)]
#[command(
    name = "extract-slice",
    about = "Extract full map data (Map 0: Eastern Kingdoms & Map 1: Kalimdor) from WoW 1.12.1 MPQs for web deployment",
    version
)]
struct Args {
    /// Path to WoW 1.12.1 Data directory holding MPQs. Defaults to $WOW_DATA.
    #[arg(short, long)]
    wow_data: Option<PathBuf>,

    /// Destination directory for the extracted assets.
    #[arg(short, long, default_value = "assets/web_slice")]
    output: PathBuf,

    /// Map IDs to extract (0 = Eastern Kingdoms / Azeroth, 1 = Kalimdor).
    /// Defaults to both Map 0 and Map 1.
    #[arg(short, long, value_delimiter = ',', default_values_t = vec![0, 1])]
    maps: Vec<u32>,

    /// Force overwrite of existing files (default skips already extracted files).
    #[arg(short, long)]
    force: bool,
}

const BASE_DBCS: &[&str] = &[
    "DBFilesClient\\Map.dbc",
    "DBFilesClient\\AreaTable.dbc",
    "DBFilesClient\\Light.dbc",
    "DBFilesClient\\LightParams.dbc",
    "DBFilesClient\\LightIntBand.dbc",
    "DBFilesClient\\LightFloatBand.dbc",
    "DBFilesClient\\LightSkybox.dbc",
    "DBFilesClient\\GroundEffectTexture.dbc",
    "DBFilesClient\\GroundEffectDoodad.dbc",
    "DBFilesClient\\ZoneIntroMusicTable.dbc",
    "DBFilesClient\\SoundWaterType.dbc",
    "DBFilesClient\\TerrainType.dbc",
    "DBFilesClient\\WMOAreaTable.dbc",
];

const BASE_WDTS: &[&str] = &[
    "World\\Maps\\Azeroth\\Azeroth.wdt",
    "World\\Maps\\Kalimdor\\Kalimdor.wdt",
    "World\\Maps\\Azeroth\\Azeroth.wdl",
    "World\\Maps\\Kalimdor\\Kalimdor.wdl",
];

fn map_id_to_name(id: u32) -> Option<&'static str> {
    match id {
        0 => Some("Azeroth"),
        1 => Some("Kalimdor"),
        _ => None,
    }
}

fn map_id_to_display_name(id: u32) -> &'static str {
    match id {
        0 => "Eastern Kingdoms (Azeroth)",
        1 => "Kalimdor",
        _ => "Custom Map",
    }
}

fn normalize_mpq_path(p: &str) -> String {
    p.replace('/', "\\")
}

fn path_for_output(base: &Path, mpq_path: &str) -> PathBuf {
    let clean = mpq_path.replace('\\', "/").trim_start_matches('/').to_string();
    base.join(clean)
}

fn extract_file(
    chain: &Chain,
    mpq_path: &str,
    output_dir: &Path,
    force: bool,
) -> Result<Option<usize>> {
    let internal = normalize_mpq_path(mpq_path);
    if !chain.contains(&internal) {
        return Ok(None);
    }

    let out_path = path_for_output(output_dir, &internal);
    if !force && out_path.exists() {
        if let Ok(meta) = fs::metadata(&out_path) {
            return Ok(Some(meta.len() as usize));
        }
    }

    let bytes = chain.read(&internal)?;
    if let Some(parent) = out_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&out_path, &bytes)?;
    Ok(Some(bytes.len()))
}

fn main() -> Result<()> {
    let args = Args::parse();

    println!("============================================================");
    println!("🏎️  BENILLA-LAMBO FULL CONTINENT EXTRACTOR");
    println!("============================================================");

    // Resolve WoW data directory
    let wow_data_path = if let Some(p) = args.wow_data {
        p
    } else if let Ok(env_val) = std::env::var("WOW_DATA") {
        PathBuf::from(env_val)
    } else {
        match benilla_formats::wow_data() {
            Some(p) => p,
            None => {
                bail!(
                    "No WoW Data directory found! Specify via --wow-data <PATH> or set $env:WOW_DATA"
                );
            }
        }
    };

    println!("• WoW Data Path:   {}", wow_data_path.display());
    println!("• Output Folder:   {}", args.output.display());
    println!("• Maps to Extract: {:?}", args.maps);
    println!("• Force Overwrite: {}", args.force);

    // Open MPQ patch chain
    println!("\n[1/5] Opening WoW 1.12.1 MPQ patch chain...");
    let chain = Chain::open(&wow_data_path)
        .with_context(|| format!("Opening MPQ chain at {}", wow_data_path.display()))?;
    println!("✓ Chain mounted successfully (Locale: {:?})", chain.locale());

    // 2. Scan entire 64x64 grid for both continents
    println!("\n[2/5] Scanning MPQs for all existing ADT tiles across requested maps...");
    let mut map_tiles: Vec<(u32, u32, u32, String)> = Vec::new(); // (map_id, tx, ty, path)

    for &map_id in &args.maps {
        let Some(map_name) = map_id_to_name(map_id) else {
            eprintln!("⚠️ Warning: Unsupported map ID {map_id}, skipping.");
            continue;
        };

        let mut continent_tiles = 0;
        for tx in 0..64 {
            for ty in 0..64 {
                let adt_path = format!("World\\Maps\\{map_name}\\{map_name}_{tx}_{ty}.adt");
                if chain.contains(&adt_path) {
                    map_tiles.push((map_id, tx, ty, adt_path));
                    continent_tiles += 1;
                }
            }
        }
        println!(
            "  • Map {}: {} -> found {} active ADT terrain tiles",
            map_id,
            map_id_to_display_name(map_id),
            continent_tiles
        );
    }

    println!("✓ Total active ADT tiles to extract: {}", map_tiles.len());

    let mut total_extracted_bytes: usize = 0;
    let mut extracted_files_count: usize = 0;
    let mut discovered_wmos: HashSet<String> = HashSet::new();
    let mut discovered_models: HashSet<String> = HashSet::new();
    let mut discovered_textures: HashSet<String> = HashSet::new();

    // 3. Extract Core DBCs and WDTs
    println!("\n[3/5] Extracting base DBC catalogs and WDT continent headers...");
    for dbc in BASE_DBCS {
        if let Some(sz) = extract_file(&chain, dbc, &args.output, args.force)? {
            total_extracted_bytes += sz;
            extracted_files_count += 1;
        }
    }
    for wdt in BASE_WDTS {
        if let Some(sz) = extract_file(&chain, wdt, &args.output, args.force)? {
            total_extracted_bytes += sz;
            extracted_files_count += 1;
        }
    }
    println!("✓ Extracted base DBC and WDT continent descriptors.");

    // 4. Extract ADT Tiles and inspect dependencies
    println!("\n[4/5] Extracting all {} ADT tiles & harvesting 3D dependencies...", map_tiles.len());
    let total_adts = map_tiles.len();
    for (idx, &(_, _, _, ref adt_path)) in map_tiles.iter().enumerate() {
        if idx % 100 == 0 || idx + 1 == total_adts {
            print!(
                "\r  Extracting ADTs: [{}/{}] ({:.1}%) ...",
                idx + 1,
                total_adts,
                ((idx + 1) as f64 / total_adts as f64) * 100.0
            );
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }

        let bytes = chain.read(adt_path)?;
        let out_path = path_for_output(&args.output, adt_path);
        if args.force || !out_path.exists() {
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&out_path, &bytes)?;
        }
        total_extracted_bytes += bytes.len();
        extracted_files_count += 1;

        // Parse ADT to find textures, WMOs, and M2s
        if let Ok(ParsedAdt::Root(root)) = parse_adt(&mut Cursor::new(&bytes)) {
            for tex in root.textures {
                if !tex.is_empty() {
                    discovered_textures.insert(tex);
                }
            }
            for wmo in root.wmos {
                if !wmo.is_empty() {
                    discovered_wmos.insert(wmo);
                }
            }
            for model in root.models {
                if !model.is_empty() {
                    discovered_models.insert(model);
                }
            }
        }
    }
    println!("\n✓ ADT terrain tiles extracted.");
    println!(
        "  Discovered: {} unique WMOs, {} unique M2 doodads, {} terrain texture paths.",
        discovered_wmos.len(),
        discovered_models.len(),
        discovered_textures.len()
    );

    // 5. Extract WMOs (Roots + Groups)
    println!("\n[5/5] Extracting WMO structures, M2 doodads, and BLP textures...");
    println!("  • Extracting {} WMO structures (and all their interior group files)...", discovered_wmos.len());
    let mut wmo_extracted = 0;
    for (i, wmo_path) in discovered_wmos.iter().enumerate() {
        if i % 100 == 0 || i + 1 == discovered_wmos.len() {
            print!(
                "\r    WMO progress: [{}/{}] ...",
                i + 1,
                discovered_wmos.len()
            );
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }

        if let Some(sz) = extract_file(&chain, wmo_path, &args.output, args.force)? {
            total_extracted_bytes += sz;
            extracted_files_count += 1;
            wmo_extracted += 1;

            if let Ok(bytes) = chain.read(wmo_path) {
                if let Ok(ParsedWmo::Root(root)) = parse_wmo(&mut Cursor::new(&bytes)) {
                    for tex in root.textures {
                        if !tex.is_empty() {
                            discovered_textures.insert(tex);
                        }
                    }

                    // Extract all group files
                    let stem = wmo_path.strip_suffix(".wmo").unwrap_or(wmo_path);
                    for g in 0..root.n_groups {
                        let group_path = format!("{stem}_{g:03}.wmo");
                        if let Some(gsz) = extract_file(&chain, &group_path, &args.output, args.force)? {
                            total_extracted_bytes += gsz;
                            extracted_files_count += 1;
                        }
                    }
                }
            }
        }
    }
    println!("\n  ✓ WMOs and group files extracted.");

    // Extract M2 models
    println!("  • Extracting {} M2 doodad models...", discovered_models.len());
    let mut m2_extracted = 0;
    for (i, m2_path) in discovered_models.iter().enumerate() {
        if i % 250 == 0 || i + 1 == discovered_models.len() {
            print!(
                "\r    M2 progress: [{}/{}] ...",
                i + 1,
                discovered_models.len()
            );
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }

        if let Some(sz) = extract_file(&chain, m2_path, &args.output, args.force)? {
            total_extracted_bytes += sz;
            extracted_files_count += 1;
            m2_extracted += 1;

            if let Ok(bytes) = chain.read(m2_path) {
                if let Ok(m2_fmt) = parse_m2(&mut Cursor::new(&bytes)) {
                    for tex in &m2_fmt.model().textures {
                        if let Ok(tex_name) = tex.filename.string.to_str() {
                            if !tex_name.is_empty() {
                                discovered_textures.insert(tex_name.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    println!("\n  ✓ M2 models extracted.");

    // Extract all textures
    println!("  • Extracting {} BLP textures...", discovered_textures.len());
    let mut blp_extracted = 0;
    for (i, tex_path) in discovered_textures.iter().enumerate() {
        if i % 500 == 0 || i + 1 == discovered_textures.len() {
            print!(
                "\r    BLP progress: [{}/{}] ...",
                i + 1,
                discovered_textures.len()
            );
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }

        if let Some(sz) = extract_file(&chain, tex_path, &args.output, args.force)? {
            total_extracted_bytes += sz;
            extracted_files_count += 1;
            blp_extracted += 1;
        }
    }
    println!("\n  ✓ BLP textures extracted.");

    let total_mb = total_extracted_bytes as f64 / (1024.0 * 1024.0);
    let total_gb = total_mb / 1024.0;
    println!("\n============================================================");
    println!("✨ FULL MAP EXTRACTION COMPLETE!");
    println!("============================================================");
    println!("• Maps Extracted:         Map 0 (Eastern Kingdoms) & Map 1 (Kalimdor)");
    println!("• Total Files Extracted:  {}", extracted_files_count);
    println!("  - Active ADT Tiles:     {}", total_adts);
    println!("  - WMO World Objects:    {}", wmo_extracted);
    println!("  - M2 Doodad Models:     {}", m2_extracted);
    println!("  - BLP Textures:         {}", blp_extracted);
    println!("• Total Extracted Size:   {:.2} MB ({:.2} GB)", total_mb, total_gb);
    println!("• Output Location:        {}", args.output.display());
    println!("============================================================");

    Ok(())
}
