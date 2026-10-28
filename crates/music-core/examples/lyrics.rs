//! Tries every lyrics provider for a search query, then the `LyricsService` pick,
//! romanization and (optionally) translation.
//!
//! ```sh
//! cargo run -p music-core --example lyrics -- "daft punk instant crush"
//! cargo run -p music-core --example lyrics -- --translate en "yoasobi idol"
//! cargo run -p music-core --example lyrics -- --raw DIR "yoasobi idol"   # + every raw payload into DIR
//! cargo run -p music-core --example lyrics -- --ja-dict DIR "yoasobi idol" # download IPADIC into DIR: kanji readings
//! ```
//!
//! Optional environment: `PAXSENIX_API_KEY`, `BETTERLYRICS_API_KEY`,
//! `AI_ENDPOINT` + `AI_API_KEY` + `AI_MODEL` (OpenAI-compatible translation
//! instead of Google).

use std::time::{Duration, Instant};

use music_core::InnerTube;
use music_core::lyrics::romanize::{Script, dictionary, romanize_result};
use music_core::lyrics::translate::{AiTranslator, GoogleTranslator, LyricsTranslator};
use music_core::lyrics::{
    LyricsQuery, LyricsResult, LyricsService, LyricsServiceConfig, ProviderId, ProviderKeys,
};

fn summary(r: &LyricsResult) -> String {
    let lines = r.synced.as_ref().map_or_else(
        || r.plain.as_deref().unwrap_or("").lines().count(),
        |l| l.lines.len(),
    );
    let extras = r.extras.iter().filter(|e| !e.is_empty()).count();
    let bg = r.extras.iter().filter(|e| e.background.is_some()).count();
    let mut s = format!("{:?} {:?}, {lines} lines", r.sync_kind(), r.format);
    if extras > 0 {
        s += &format!(", {extras} with extras ({bg} background vocals)");
    }
    if !r.agents.is_empty() {
        s += &format!(", {} agents", r.agents.len());
    }
    s
}

fn first_lines(r: &LyricsResult, n: usize) -> Vec<String> {
    match &r.synced {
        Some(l) => l
            .lines
            .iter()
            .take(n)
            .map(|line| {
                format!(
                    "{:>7.2}–{:>7.2} {} [{} words]",
                    line.start.as_secs_f32(),
                    line.end.as_secs_f32(),
                    line.text,
                    line.words.len()
                )
            })
            .collect(),
        None => r
            .plain
            .as_deref()
            .unwrap_or("")
            .lines()
            .take(n)
            .map(|l| format!("{:>16}{l}", ""))
            .collect(),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut translate_to = None;
    if let Some(i) = args.iter().position(|a| a == "--translate") {
        args.remove(i);
        if i < args.len() {
            translate_to = Some(args.remove(i));
        }
    }
    let mut raw_dir = None;
    if let Some(i) = args.iter().position(|a| a == "--raw") {
        args.remove(i);
        if i < args.len() {
            raw_dir = Some(std::path::PathBuf::from(args.remove(i)));
        }
    }
    if let Some(i) = args.iter().position(|a| a == "--ja-dict") {
        args.remove(i);
        if i < args.len() {
            let dir = std::path::PathBuf::from(args.remove(i));
            let source = dictionary::DictionarySource::ipadic();
            let t = Instant::now();
            let mut last = 0;
            let path = dictionary::install(
                &source,
                &dir,
                &std::sync::atomic::AtomicBool::new(false),
                |done, total| {
                    if done == total || done - last >= 2 << 20 {
                        last = done;
                        eprintln!("dictionary: {done} / {total} bytes");
                    }
                },
            )?;
            dictionary::load(&path)?;
            println!(
                "kanji dictionary {} ready in {} ms ({} bytes on disk)\n",
                path.display(),
                t.elapsed().as_millis(),
                dictionary::installed_size(&source, &dir)
            );
        }
    }
    let track = InnerTube::default()
        .search_songs(&args.join(" "))?
        .into_iter()
        .next()
        .ok_or("no results")?;
    println!(
        "{} — {} [{:?}] {:?}s ({})\n",
        track.title, track.artists, track.album, track.duration_secs, track.video_id
    );
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let keys = ProviderKeys {
        paxsenix: env("PAXSENIX_API_KEY"),
        betterlyrics: env("BETTERLYRICS_API_KEY"),
    };
    let mut query = LyricsQuery::from_track(&track);
    query.timeout = Some(Duration::from_secs(15));

    for id in ProviderId::ALL {
        let provider = id.create(&keys);
        if !provider.is_configured() {
            println!("{:<22} skipped (no API key)", provider.name());
            continue;
        }
        let t = Instant::now();
        let res = provider.fetch(&query);
        let ms = t.elapsed().as_millis();
        match res {
            Ok(Some(r)) => {
                println!("{:<22} {ms:>5} ms  {}", provider.name(), summary(&r));
                if let Some(dir) = &raw_dir {
                    std::fs::create_dir_all(dir)?;
                    std::fs::write(dir.join(format!("{}.txt", id.key())), &r.raw)?;
                }
                for l in first_lines(&r, 2) {
                    println!("{:<31}{l}", "");
                }
            }
            Ok(None) => println!("{:<22} {ms:>5} ms  not found", provider.name()),
            Err(e) => println!("{:<22} {ms:>5} ms  error: {e}", provider.name()),
        }
    }

    let service = LyricsService::new(LyricsServiceConfig {
        keys,
        ..LyricsServiceConfig::default()
    });
    let t = Instant::now();
    let Some(best) = service.fetch(&LyricsQuery::from_track(&track))? else {
        println!("\nLyricsService: no lyrics");
        return Ok(());
    };
    println!(
        "\nLyricsService picked {} in {} ms: {}",
        best.provider,
        t.elapsed().as_millis(),
        summary(&best)
    );

    let romanized = romanize_result(&best, Script::Auto);
    let texts: Vec<String> = match &best.synced {
        Some(l) => l.lines.iter().map(|l| l.text.clone()).collect(),
        None => best
            .plain
            .as_deref()
            .unwrap_or("")
            .lines()
            .map(str::to_owned)
            .collect(),
    };
    let translations = translate_to.as_deref().map(|lang| {
        let lines: Vec<&str> = texts.iter().map(String::as_str).collect();
        let t = Instant::now();
        let result = match (env("AI_ENDPOINT"), env("AI_API_KEY"), env("AI_MODEL")) {
            (Some(e), Some(k), Some(m)) => AiTranslator::new(e, k, m).translate_lines(&lines, lang),
            _ => GoogleTranslator::default().translate_lines(&lines, lang),
        };
        println!(
            "translated {} lines in {} ms",
            lines.len(),
            t.elapsed().as_millis()
        );
        result.unwrap_or_else(|e| {
            println!("translation failed: {e}");
            vec![None; lines.len()]
        })
    });
    for (i, text) in texts.iter().enumerate().take(8) {
        println!("  {text}");
        if let Some(r) = &romanized[i] {
            println!("    romanized: {r}");
        }
        if let Some(t) = translations.as_ref().and_then(|t| t[i].as_ref()) {
            println!("    translated: {t}");
        }
    }
    Ok(())
}
