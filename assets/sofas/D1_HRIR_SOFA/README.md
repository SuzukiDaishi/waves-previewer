# SADIE II HRIRs, subject D1 (KU100 dummy head)

Head-related impulse responses of the KU100 dummy head (subject D1), from the
SADIE II database by the AudioLab, Department of Electronic Engineering,
University of York: <https://www.york.ac.uk/sadie-project/database.html>.
These are the database's `D1_HRIR_SOFA` files, unmodified.

| File | Rate / depth | Taps |
|---|---|---|
| `D1_44K_16bit_256tap_FIR_SOFA.sofa` | 44.1 kHz / 16-bit | 256 |
| `D1_48K_24bit_256tap_FIR_SOFA.sofa` | 48 kHz / 24-bit | 256 |
| `D1_96K_24bit_512tap_FIR_SOFA.sofa` | 96 kHz / 24-bit | 512 (not committed; see below) |

Each holds 8802 source positions, diffuse-field equalised, low-frequency
extended and windowed (approximately linear phase), as the files' own
`Comment` attribute describes.

## Licence

Copyright 2018, University of York. Licensed under the Apache License,
Version 2.0. The full text, with the University's notice and the database's
own terms, is in
[`assets/licenses/texts/Apache-2.0-SADIE-II.txt`](../../licenses/texts/Apache-2.0-SADIE-II.txt),
and the in-app Help -> Licenses window lists this data from
`assets/licenses/extra.json`.

The database asks that the original dataset be referenced whenever it is
used, in original or modified form. For academic work, cite:

> C. Armstrong, L. Thresh, D. Murphy and G. Kearney, "A Perceptual Evaluation
> of Individual and Non-Individual HRTFs: A Case Study of the SADIE II
> Database", *Applied Sciences* 8(11), 2029 (2018).
> <https://doi.org/10.3390/app8112029>

The installer copies `D1_48K_24bit_256tap_FIR_SOFA.sofa` to `hrtf\` beside
NeoWaves, where it is the bundled HRTF for headphone monitoring (resampled to
the output's rate when that is not 48 kHz); a development build reads it from
here. The 44.1 kHz file stays in the repository for the tests (a second
profile to switch to). The 96 kHz file (72 MB) is used by nothing, so it is
kept out of git (`.gitignore`); download `D1_HRIR_SOFA.zip` from the database
page above and put it here if you want it. Whatever else comes to ship these
files must carry the licence entry above with them.
