use super::*;

const RATE: u32 = 48_000;

fn on(preset: ReverbPreset) -> ReverbSettings {
    ReverbSettings::new(preset, 0.5)
}

/// Deterministic white noise in -1..1.
fn noise(frames: usize, seed: u32) -> Vec<f32> {
    let mut x = seed.max(1);
    (0..frames * 2)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x as f32 / u32::MAX as f32) * 2.0 - 1.0
        })
        .collect()
}

/// Runs `input` through in 512-frame blocks, like the deck.
fn run(reverb: &mut Reverb, input: &mut [f32]) {
    for block in input.chunks_mut(1024) {
        reverb.process(block);
    }
}

/// Seconds until the tail of an impulse stays below -60 dB of its peak.
fn decay_secs(preset: ReverbPreset) -> f32 {
    decay_of(ReverbSettings::new(preset, 1.0))
}

fn decay_of(settings: ReverbSettings) -> f32 {
    let mut reverb = Reverb::new(RATE, settings);
    // Let the wet gain settle first.
    run(&mut reverb, &mut vec![0.0; 2 * RATE as usize / 10]);
    let mut buf = vec![0.0; 2 * RATE as usize * 12];
    buf[0] = 1.0;
    buf[1] = 1.0;
    run(&mut reverb, &mut buf);
    let wet: Vec<f32> = buf
        .as_chunks::<2>()
        .0
        .iter()
        .skip(1)
        .map(|f| f[0].abs())
        .collect();
    let peak = wet.iter().fold(0.0f32, |a, &b| a.max(b));
    let floor = peak * 1e-3;
    let last = wet.iter().rposition(|&v| v > floor).unwrap_or(0);
    last as f32 / RATE as f32
}

#[test]
fn silence_in_silence_out() {
    for preset in ReverbPreset::ALL {
        let mut reverb = Reverb::new(RATE, on(preset));
        let mut buf = vec![0.0; 2 * RATE as usize];
        run(&mut reverb, &mut buf);
        assert!(buf.iter().all(|&s| s == 0.0), "{preset:?}");
        assert!(!reverb.is_ringing());
    }
}

#[test]
fn noise_stays_bounded_and_finite() {
    for preset in ReverbPreset::ALL {
        let mut reverb = Reverb::new(RATE, ReverbSettings::new(preset, 1.0));
        let mut buf = noise(RATE as usize * 20, 7);
        run(&mut reverb, &mut buf);
        let peak = buf.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(buf.iter().all(|s| s.is_finite()), "{preset:?}");
        assert!(peak < 4.0, "{preset:?}: peak {peak}");
        let (state, _) = reverb.state_stats();
        assert!(state < 20.0, "{preset:?}: state {state}");
    }
}

#[test]
fn tail_dies_out_without_denormals() {
    let mut reverb = Reverb::new(RATE, ReverbSettings::new(ReverbPreset::Cathedral, 1.0));
    let mut buf = noise(RATE as usize, 3);
    run(&mut reverb, &mut buf);
    assert!(reverb.is_ringing());
    // Long silence: the tail decays, is flushed to exact zeros and ends.
    let mut checked = false;
    for _ in 0..60 {
        let mut silence = vec![0.0; 2 * RATE as usize];
        run(&mut reverb, &mut silence);
        let (_, subnormal) = reverb.state_stats();
        assert!(!subnormal, "a delay line holds a subnormal value");
        assert!(silence.iter().all(|s| !s.is_subnormal()));
        if !reverb.is_ringing() {
            assert!(silence.last().is_some_and(|&s| s == 0.0));
            checked = true;
            break;
        }
    }
    assert!(checked, "the tail never ended");
    let (max, _) = reverb.state_stats();
    assert_eq!(max, 0.0, "lines cleared after the tail");
}

#[test]
fn presets_differ_in_length() {
    let small = decay_secs(ReverbPreset::SmallRoom);
    let hall = decay_secs(ReverbPreset::Hall);
    let cathedral = decay_secs(ReverbPreset::Cathedral);
    assert!(
        small < hall && hall < cathedral,
        "{small} {hall} {cathedral}"
    );
    assert!(small > 0.2 && cathedral < 12.0, "{small} {cathedral}");
    // Plate: bright (least damping), no pre-delay.
    let plate = ReverbPreset::Plate.params().unwrap();
    for p in [
        ReverbPreset::SmallRoom,
        ReverbPreset::Hall,
        ReverbPreset::Cathedral,
    ] {
        assert!(plate.damping < p.params().unwrap().damping);
    }
    assert_eq!(plate.pre_delay_ms, 0.0);
    assert!(ReverbPreset::Off.params().is_none());
}

#[test]
fn pre_delay_holds_the_first_echo_back() {
    let mut reverb = Reverb::new(RATE, ReverbSettings::new(ReverbPreset::Cathedral, 1.0));
    run(&mut reverb, &mut vec![0.0; 2 * 4800]);
    let mut buf = vec![0.0; 2 * RATE as usize / 4];
    buf[0] = 1.0;
    run(&mut reverb, &mut buf);
    let first = buf
        .as_chunks::<2>()
        .0
        .iter()
        .skip(1)
        .position(|f| f[0] != 0.0 || f[1] != 0.0)
        .unwrap()
        + 1;
    // 45 ms pre-delay + the shortest comb (1116 / 44.1 kHz ≈ 25 ms).
    let ms = first as f32 * 1000.0 / RATE as f32;
    assert!((65.0..75.0).contains(&ms), "first echo after {ms} ms");
}

#[test]
fn delay_lines_follow_the_rate() {
    let a = Reverb::new(44_100, on(ReverbPreset::Hall));
    let b = Reverb::new(88_200, on(ReverbPreset::Hall));
    assert_eq!(a.comb_len(0), 1116);
    assert_eq!(b.comb_len(0), 2232);
}

#[test]
fn turning_off_glides_to_dry_and_finishes() {
    let mut reverb = Reverb::new(RATE, on(ReverbPreset::Hall));
    let mut buf = noise(RATE as usize / 2, 9);
    run(&mut reverb, &mut buf);
    assert!(!reverb.is_finished());
    reverb.set(ReverbSettings::new(ReverbPreset::Off, 0.5));
    let input = noise(RATE as usize / 2, 11);
    let mut buf = input.clone();
    run(&mut reverb, &mut buf);
    assert!(reverb.is_finished());
    // At the end the output is the input again.
    let tail = buf.len() - 200;
    for (o, i) in buf[tail..].iter().zip(&input[tail..]) {
        assert!((o - i).abs() < 1e-3, "{o} vs {i}");
    }
}

#[test]
fn settings_sanitize_and_parse() {
    assert!(!ReverbSettings::default().is_on());
    assert!(!ReverbSettings::new(ReverbPreset::Hall, 0.0).is_on());
    assert_eq!(ReverbSettings::new(ReverbPreset::Hall, 3.0).mix, 1.0);
    assert_eq!(
        ReverbSettings::new(ReverbPreset::Hall, f32::NAN).mix,
        DEFAULT_REVERB_MIX
    );
    for p in ReverbPreset::ALL {
        assert_eq!(ReverbPreset::parse(p.key()), Some(p));
    }
    assert_eq!(
        ReverbPreset::parse("Small room"),
        Some(ReverbPreset::SmallRoom)
    );
    assert_eq!(ReverbPreset::parse("spring"), None);
}

/// Offline cost: `cargo test --release -p music-core reverb_cost -- --ignored --nocapture`.
#[test]
#[ignore = "timing, run by hand"]
fn reverb_cost() {
    let input = noise(RATE as usize * 60, 5);
    let fine = ReverbSettings::new(ReverbPreset::Custom, 0.4).with_params(ReverbParams {
        early: 0.6,
        pre_delay_ms: 120.0,
        ..ReverbParams::default()
    });
    let settings = ReverbPreset::ALL
        .map(|p| (format!("{p:?}"), ReverbSettings::new(p, 0.4)))
        .into_iter()
        .chain([("Custom, early reflections".to_owned(), fine)]);
    for (preset, settings) in settings {
        let mut reverb = Reverb::new(RATE, settings);
        let mut buf = input.clone();
        let started = std::time::Instant::now();
        run(&mut reverb, &mut buf);
        let took = started.elapsed();
        println!(
            "{preset}: 60 s of 48 kHz stereo in {took:?} ({:.3} % of one core)",
            took.as_secs_f64() / 60.0 * 100.0
        );
    }
}

fn rms(buf: &[f32]) -> f32 {
    (buf.iter().map(|v| v * v).sum::<f32>() / buf.len().max(1) as f32).sqrt()
}

/// The wet part alone for `input` (dry level 0, mix 1).
fn wet_only(settings: ReverbSettings, input: &[f32]) -> Vec<f32> {
    let p = ReverbParams {
        dry: 0.0,
        ..settings.params
    };
    let mut reverb = Reverb::new(RATE, settings.with_params(p));
    run(&mut reverb, &mut vec![0.0; 2 * RATE as usize / 10]);
    let mut buf = input.to_vec();
    run(&mut reverb, &mut buf);
    buf
}

#[test]
fn rooms_are_presets_of_the_fine_controls() {
    // The four rooms keep Freeverb's feedback for their room size.
    for (preset, room_size) in [
        (ReverbPreset::SmallRoom, 0.42),
        (ReverbPreset::Hall, 0.80),
        (ReverbPreset::Plate, 0.70),
        (ReverbPreset::Cathedral, 0.95),
    ] {
        let p = preset.params().unwrap();
        let old = room_size * SCALE_ROOM + OFFSET_ROOM;
        assert!((p.feedback() - old).abs() < 1e-5, "{preset:?}");
        assert_eq!((p.early, p.dry), (0.0, 1.0));
        assert!(DECAY_RANGE.contains(&p.decay_secs), "{preset:?}");
    }
    // The same values by hand ("Custom") sound the same as the room.
    let hall = ReverbSettings::new(ReverbPreset::Hall, 0.5);
    let custom = ReverbSettings::new(ReverbPreset::Custom, 0.5).with_params(hall.params);
    let input = noise(RATE as usize / 2, 4);
    let (mut a, mut b) = (input.clone(), input);
    run(&mut Reverb::new(RATE, hall), &mut a);
    run(&mut Reverb::new(RATE, custom), &mut b);
    assert_eq!(a, b);
}

#[test]
fn length_sets_the_decay() {
    let at = |decay_secs: f32| {
        decay_of(
            ReverbSettings::new(ReverbPreset::Custom, 1.0).with_params(ReverbParams {
                decay_secs,
                damping: 0.0,
                ..ReverbParams::default()
            }),
        )
    };
    let (short, mid, long) = (at(0.3), at(2.0), at(8.0));
    assert!(short < mid && mid < long, "{short} {mid} {long}");
    // Undamped, −60 dB after about the set time (the allpasses add a little).
    assert!((1.4..2.8).contains(&mid), "{mid}");
    assert!(short < 0.7, "{short}");
}

#[test]
fn brightness_keeps_more_highs() {
    // Highs: the sample-to-sample difference of the tail; damped tails are smoother.
    let highs = |damping: f32| {
        let s = ReverbSettings::new(ReverbPreset::Custom, 1.0).with_params(ReverbParams {
            damping,
            ..ReverbParams::default()
        });
        let mut impulse = vec![0.0; 2 * RATE as usize];
        impulse[0] = 1.0;
        impulse[1] = 1.0;
        let out = wet_only(s, &impulse);
        let tail: Vec<f32> = out[RATE as usize / 2..]
            .iter()
            .step_by(2)
            .copied()
            .collect();
        let diff: Vec<f32> = tail.windows(2).map(|w| w[1] - w[0]).collect();
        rms(&diff) / rms(&tail)
    };
    assert!(
        highs(0.0) > highs(1.0) * 1.2,
        "{} {}",
        highs(0.0),
        highs(1.0)
    );
}

#[test]
fn long_pre_delay_and_early_reflections() {
    let first_sound = |params: ReverbParams| {
        let s = ReverbSettings::new(ReverbPreset::Custom, 1.0).with_params(params);
        let mut impulse = vec![0.0; 2 * RATE as usize / 2];
        impulse[0] = 1.0;
        impulse[1] = 1.0;
        let out = wet_only(s, &impulse);
        let at = out
            .as_chunks::<2>()
            .0
            .iter()
            .position(|f| f[0] != 0.0 || f[1] != 0.0)
            .unwrap();
        at as f32 * 1000.0 / RATE as f32
    };
    let base = ReverbParams {
        pre_delay_ms: 200.0,
        ..ReverbParams::default()
    };
    // 200 ms + the shortest comb (~25 ms).
    let ms = first_sound(base);
    assert!((224.0..228.0).contains(&ms), "{ms}");
    // Early reflections start 4.3 ms after the pre-delay, well before the tail.
    let ms = first_sound(ReverbParams { early: 1.0, ..base });
    assert!((204.0..205.0).contains(&ms), "{ms}");
}

#[test]
fn dry_level_lowers_the_song() {
    let s = ReverbSettings::new(ReverbPreset::Hall, 0.5);
    let (dry_full, _) = s.gains();
    let half = s.clone().with_params(ReverbParams {
        dry: 0.5,
        ..s.params
    });
    assert!((half.gains().0 - dry_full * 0.5).abs() < 1e-6);
    // Dry 0: an impulse is gone from the first frame (only the tail is left).
    let mut reverb = Reverb::new(
        RATE,
        s.with_params(ReverbParams {
            dry: 0.0,
            ..ReverbParams::default()
        }),
    );
    run(&mut reverb, &mut vec![0.0; 2 * RATE as usize / 10]);
    let mut buf = vec![0.0; 2 * 64];
    buf[0] = 1.0;
    buf[1] = 1.0;
    reverb.process(&mut buf);
    assert_eq!(buf[0], 0.0);
}

#[test]
fn pre_delay_changes_glide_without_jumps() {
    let s = ReverbSettings::new(ReverbPreset::Custom, 1.0);
    let mut reverb = Reverb::new(RATE, s.clone());
    run(&mut reverb, &mut noise(RATE as usize / 4, 2));
    reverb.set(s.with_params(ReverbParams {
        pre_delay_ms: 150.0,
        ..ReverbParams::default()
    }));
    let target = reverb.pre_target;
    let before = reverb.pre_len;
    reverb.process(&mut noise(64, 3));
    assert_eq!(reverb.pre_len, before + 64);
    run(&mut reverb, &mut noise(RATE as usize / 2, 3));
    assert_eq!(reverb.pre_len, target);
}

#[test]
fn convolution_rooms_ring_bounded_and_end_clean() {
    for preset in ReverbPreset::IMPULSE_ROOMS {
        let mut reverb = Reverb::new(RATE, ReverbSettings::new(preset, 1.0));
        assert_eq!(
            reverb.kind(),
            TailKind::Room(preset.impulse_room().unwrap())
        );
        // Silence in: exact silence, nothing ringing.
        let mut buf = vec![0.0; 2 * RATE as usize / 2];
        run(&mut reverb, &mut buf);
        assert!(buf.iter().all(|&s| s == 0.0) && !reverb.is_ringing());
        let mut buf = noise(RATE as usize * 3, 7);
        run(&mut reverb, &mut buf);
        let peak = buf.iter().fold(0.0f32, |a, &b| a.max(b.abs()));
        assert!(
            buf.iter().all(|s| s.is_finite()) && peak < 4.0,
            "{preset:?} {peak}"
        );
        // The tail ends and the convolver holds exact zeros again.
        let mut ended = false;
        for _ in 0..10 {
            let mut silence = vec![0.0; 2 * RATE as usize];
            run(&mut reverb, &mut silence);
            assert!(silence.iter().all(|s| !s.is_subnormal()));
            if !reverb.is_ringing() {
                assert_eq!(*silence.last().unwrap(), 0.0);
                ended = true;
                break;
            }
        }
        assert!(ended, "{preset:?} never stopped ringing");
        assert_eq!(reverb.convolver_clear(), Some(true));
    }
}

#[test]
fn convolution_rooms_are_about_as_loud_as_the_hall() {
    let input = noise(RATE as usize * 2, 13);
    let level = |s: ReverbSettings| {
        let out = wet_only(s, &input);
        rms(&out[out.len() / 2..])
    };
    let hall = level(ReverbSettings::new(ReverbPreset::Hall, 1.0));
    for preset in ReverbPreset::IMPULSE_ROOMS {
        let room = level(ReverbSettings::new(preset, 1.0));
        let db = 20.0 * (room / hall).log10();
        assert!(db.abs() < 4.0, "{preset:?}: {db:+.1} dB against the Hall");
    }
}

#[test]
fn own_impulse_needs_one_and_follows_it() {
    let off = ReverbSettings::new(ReverbPreset::OwnImpulse, 0.5);
    assert!(!off.is_on());
    assert_eq!(off.tail_kind(), None);
    let mut mono = vec![0.0; 2_000];
    mono[0] = 1.0;
    mono[700] = 0.5;
    let imp = Arc::new(Impulse::from_interleaved(RATE, 1, &mono).unwrap());
    let on = off.with_impulse(Some(imp.clone()));
    assert!(on.is_on());
    assert_eq!(
        on.tail_kind(),
        Some(TailKind::Own(Arc::as_ptr(&imp) as usize))
    );
    // A click comes back as the impulse's two taps, 512 frames late (the block).
    let mut buf = vec![0.0; 2 * 4_000];
    buf[0] = 1.0;
    buf[1] = 1.0;
    let out = wet_only(on, &buf);
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    let loud: Vec<usize> = left
        .iter()
        .enumerate()
        .filter(|(_, v)| v.abs() > 0.01)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(loud, vec![512, 1212], "{loud:?}");
    assert!((left[1212] / left[512] - 0.5).abs() < 1e-3);
}
