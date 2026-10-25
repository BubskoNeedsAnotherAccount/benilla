# Reference recordings

Real engines, used only to *measure* the sound model against (spectra, line structure, noise floor, level versus
load - see `tools/sound/` and the Audio section of the top-level README). They are not played back and not copied
into the sound model; the `.ogg` files themselves are git-ignored, only this list is kept.

| file | what | length | source and license |
|---|---|---|---|
| `maserati_granturismo_s.ogg` | Maserati GranTurismo S 4.7 V8: starts, idles, revs and blips the throttle; close external recording, rolls off below ~120 Hz | 66 s | Wikimedia Commons, `File:Maserati GranTurismo S Exhaust.ogg`; author lmartins (via Freesound); CC BY 4.0 |
| `bmw_m70_v12_idle.ogg` | BMW M70 5.0 L V12 idling (~720 rpm), recorded close to the engine | 4.4 s | Commons, `File:M70.ogg`; author Johannes Maximilian; CC BY-SA 4.0 |
| `chrysler_225_slant6.ogg` | 1965 Chrysler Valiant 225 slant-six: cranks, fires, idles at ~1270 and ~1680 rpm, shuts off; near-field | 9 s | Commons, `File:225 Slant Six.ogg`; author Scheinwerfermann; CC BY-SA 4.0 |
| `trophy_truck_and_v8s.ogg` | Jesse James' Trophy Truck idling and other American V8s, Goodwood Festival of Speed 2009; spectator distance | 82 s | Commons, `File:Trophy Truck and American V8s.ogg`; author Edvvc; CC BY-SA 3.0 |

Decode them with `tools/sound/oggdec` (a 40-line Rust converter on the `lewton` crate that is already in the Bevy
dependency tree) and look at them with the Python tools next to it.

What the recordings are and are not: four different cars, microphones, distances and recorders. They fix the shape of
real spectra (envelope slope, how noisy the top end is, how clean the harmonic lines are, how the top end grows with
load) but not the absolute level or the low end (two are high-passed or near-field), and none is a measured, calibrated
exhaust test. The Maserati, the only external, exhaust-dominated recording of a V8, is the primary target; the others
check that the same effects show up elsewhere.

## batch2 (2026-10-09, `batch2/`, git-ignored like the others)

Downloaded for the turbo, burble, low-end and combustion-noise work, with the user's permission, from Wikimedia Commons
(analysis only, not redistributed; the `.ogg`/`.wav` are ignored by git). Decoded with `oggdec` into `target/soundlab/ref2/`.

| file | what | source and license |
|---|---|---|
| `celica_st185.ogg`, `celica_st205.ogg` | Toyota Celica GT-Four rally cars (turbo, anti-lag): the bangs of the overrun, flutter chirps | Commons, `Toyota WRC Celica ST185 GT-Four (1992).ogg`, `...ST205 GT-Four (1995).ogg`; Edvvc; CC BY-SA 3.0 |
| `focus_rs_wrc_2010.ogg`, `audi_quattro_s1.ogg` | Ford Focus RS WRC (2010), Audi Sport Quattro S1 (1985): turbo rally cars, pops, anti-lag whoosh | Commons; Edvvc; CC BY-SA 3.0 |
| `renault5_maxi_turbo.ogg` | Renault 5 Maxi Turbo: a turbo whistle at 3.3 kHz (line power -10 dB re total, 30 dB over its surroundings) | Commons; Edvvc; CC BY-SA 3.0 |
| `saab900_turbo_coldstart.wav` | Saab 900 16V Turbo cold start and idle (turbo at rest) | Commons, `Saab 900 16V Turbo Kaltstart.wav`; KunstKonst; CC BY-SA 4.0 |
| `veyron_pur_sang.ogg`, `veyron_grand_sport.ogg` | Bugatti Veyron (W16) idling and revving: low-body (30-200 over 200-1500 Hz) -2..+3 dB, which the W16's low-end floor is set to | Commons; Edvvc; CC BY-SA 3.0 |
| `peugeot908_diesel_v12.ogg` | Peugeot 908 HDi FAP (V12 diesel racer) | Commons; Edvvc; CC BY-SA 3.0 |
| `aston_v12.ogg`, `zonda_v12.ogg`, `lotus_evora.ogg` | Aston Martin V12 Vantage, Pagani Zonda Roadster F, Lotus Evora (V12, V12, V6) at the Goodwood Festival of Speed | Commons; Edvvc; CC BY-SA 3.0 |
| `cayman_s.wav`, `five_cyl.ogg`, `lexus_isf_v8_dyno.ogg` | Porsche Cayman S (flat-6), a five-cylinder, Lexus IS-F V8 on a dyno | Commons; John64 CC BY-SA 3.0, Jonas Tittmann CC BY-SA 4.0, Altair78 CC BY-SA 4.0 |

What they gave: the statistics of real pops (bursts of 30-80 ms, the 99.9th percentile of the 1.5-8 kHz envelope 25-37 dB over its
median, a decay of 1-7 ms; `tools/sound/pops.py`, `hfcrest.py`), the shape of turbo flutter (a train of chirps, each a tone sweeping up by
2-3x and back over ~0.1 s at 3-12 per second), and the low end of a W16.
