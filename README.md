# NeoWaves Audio List Editor (NeoWaves)

螟夜Κ繝輔か繝ｫ繝繧貞・蟶ｰ逧・↓襍ｰ譟ｻ縺励※ `.wav` 繧剃ｸ隕ｧ陦ｨ遉ｺ縺励ヾpace 縺ｧ蜊ｳ隧ｦ閨ｴ縲∵ｳ｢蠖｢(min/max)縺ｨ dBFS 繝｡繝ｼ繧ｿ繧定｡ｨ遉ｺ縺吶ｋ Rust 陬ｽ縺ｮ霆ｽ驥上・繝ｬ繝薙Η繝ｯ繝ｼ縺ｧ縺吶・UI 縺ｯ `eframe/egui`縲√が繝ｼ繝・ぅ繧ｪ蜃ｺ蜉帙・ `cpal` 繧剃ｽｿ逕ｨ縺励※縺・∪縺吶ゅΜ繧ｹ繝医・螟ｧ隕乗ｨ｡縺ｧ繧りｻｽ蠢ｫ縺ｫ蜍穂ｽ懊＠縲∵､懃ｴ｢繝ｻ繧ｽ繝ｼ繝医・蜊ｳ譎ゅ・繝ｬ繝薙Η繝ｼ縺ｫ譛驕ｩ蛹悶＠縺ｦ縺・∪縺吶・

迴ｾ迥ｶ縺ｯ WAV 縺ｮ縺ｿ蟇ｾ蠢懶ｼ・hound`・峨ゆｻ雁ｾ・`symphonia` 縺ｫ繧医ｋ mp3/ogg/flac/aac 蟇ｾ蠢懊ｒ莠亥ｮ壹＠縺ｦ縺・∪縺吶・

## Recent Updates
- Added Settings > Appearance (Dark/Light). Default is Dark and it persists across restarts.
- Theme preference is stored in `%APPDATA%\\NeoWaves\\prefs.txt`.
- Undo/Redo in the editor (Ctrl+Z / Ctrl+Ehift+Z) with toolbar buttons.
- List UX: click selection no longer auto-centers; keyboard selection still auto-centers.
- Metadata loading prioritizes visible rows when you jump-scroll.

## 迴ｾ迥ｶ縺ｮ逕ｻ髱｢繧､繝｡繝ｼ繧ｸ
![](docs/gamen_a.png)
![](docs/gamen_b.png)

---

## Documentation

See docs index for full guides and references:

- docs/INDEX.md

---

## 繝・じ繧､繝ｳ譁ｹ驥晢ｼ医＃謠先｡茨ｼ・

蝓ｺ譛ｬ逕ｻ髱｢縺ｯ縲後Μ繧ｹ繝郁｡ｨ遉ｺ縲阪ゅヵ繧｡繧､繝ｫ蜷阪ｒ繝繝悶Ν繧ｯ繝ｪ繝・け縺吶ｋ縺ｨ縲梧ｳ｢蠖｢繧ｨ繝・ぅ繧ｿ縲阪ｒ髢九″縺ｾ縺吶・

縺ｩ縺｡繧峨・陦ｨ遉ｺ譁ｹ豕輔′繧医＞縺区､懆ｨ惹ｸｭ縺ｧ縺吶′縲∝・譛溷ｮ溯｣・・縲悟酔荳繧ｦ繧｣繝ｳ繝峨え蜀・ち繝悶阪ｒ謗｡逕ｨ縺励∪縺呻ｼ亥ｮ溯｣・ｮｹ譏薙・荳菴捺─繝ｻ繧ｷ繝ｧ繝ｼ繝医き繝・ヨ縺檎ｴ逶ｴ・峨ょｰ・擂逧・↓繝昴ャ繝励い繧ｦ繝茨ｼ亥挨繧ｦ繧｣繝ｳ繝峨え・峨ｂ驕ｸ縺ｹ繧玖ｨｭ險医↓縺励∪縺吶・

- 繧ｿ繝匁婿蠑擾ｼ域里螳夲ｼ・ 1 繧ｦ繧｣繝ｳ繝峨え蜀・〒隍・焚繧ｨ繝・ぅ繧ｿ繧偵ち繝門・譖ｿ縲り､・焚繝輔ぃ繧､繝ｫ豈碑ｼ・′讌ｽ縲ゅ・繝ｫ繝√Δ繝九ち蛻ｩ逕ｨ譎ゅ・蠕瑚ｿｰ縺ｮ繝昴ャ繝励い繧ｦ繝医〒陬懷ｮ後・
- 蛻･繧ｦ繧｣繝ｳ繝峨え譁ｹ蠑擾ｼ亥ｰ・擂繧ｪ繝励す繝ｧ繝ｳ・・ 繧ｨ繝・ぅ繧ｿ繧呈眠隕上え繧｣繝ｳ繝峨え縺ｸ蛻・屬縲ゅ・繝ｫ繝√Δ繝九ち縺ｧ荳ｦ縺ｹ繧峨ｌ繧倶ｸ譁ｹ縲√え繧｣繝ｳ繝峨え邂｡逅・・隍・尅縺輔′蠅励＠縺ｾ縺吶・

### 繝｢繝・け・亥盾閠・ｼ・

繝ｪ繧ｹ繝育判髱｢・井ｸ隕ｧ・・

![list](docs/隕∽ｻｶ螳夂ｾｩ_繝ｪ繧ｹ繝・png)

豕｢蠖｢繧ｨ繝・ぅ繧ｿ・郁ｩｳ邏ｰ・・

![editor](docs/隕∽ｻｶ螳夂ｾｩ_豕｢蠖｢繧ｨ繝・ぅ繧ｿ.png)

---

## 讖溯・

- 繝輔か繝ｫ繝驕ｸ謚橸ｼ亥・蟶ｰ襍ｰ譟ｻ・峨〒 `.wav` 繧剃ｸ隕ｧ陦ｨ遉ｺ・井ｸ企Κ繝舌・縺ｫ邱乗焚繧定｡ｨ遉ｺ縲∬ｪｭ縺ｿ霎ｼ縺ｿ荳ｭ縺ｯ 竢ｳ 陦ｨ遉ｺ・・
- 讀懃ｴ｢繝舌・縺ｧ繝輔ぃ繧､繝ｫ蜷・繝輔か繝ｫ繝繧帝Κ蛻・ｸ閾ｴ繝輔ぅ繝ｫ繧ｿ・郁｡ｨ遉ｺ謨ｰ/邱乗焚繧定｡ｨ遉ｺ・・
- 繝輔ぃ繧､繝ｫ蜷阪・繝繝悶Ν繧ｯ繝ｪ繝・け縺ｧ繧ｨ繝・ぅ繧ｿ繧ｿ繝悶ｒ髢九￥・亥酔荳繧ｦ繧｣繝ｳ繝峨え蜀・ｼ・
- Space/繝懊ち繝ｳ縺ｧ蜀咲函繝ｻ蛛懈ｭ｢縲・浹驥上せ繝ｩ繧､繝縲‥BFS 繝｡繝ｼ繧ｿ陦ｨ遉ｺ
- 繧ｨ繝・ぅ繧ｿ縺ｮ Undo/Redo・・trl+Z / Ctrl+Ehift+Z・峨・nspector 縺ｫ繝懊ち繝ｳ繧り｡ｨ遉ｺ
- 繝｢繝ｼ繝蛾∈謚橸ｼ・ode: Speed / PitchShift / TimeStretch・・
  - Speed: 蜀咲函騾溷ｺｦ・・peed x [0.25縲・.0]・峨ゅヴ繝・メ縺ｯ螟牙喧・磯撼菫晄戟・峨ゅΜ繧｢繝ｫ繧ｿ繧､繝蜀咲函縺ｧ菴朱≦蟒ｶ縲・
  - PitchShift: 繧ｻ繝溘ヨ繝ｼ繝ｳ・・12縲・12・峨〒繝斐ャ繝√・縺ｿ螟画峩縲る聞縺輔・菫晄戟縲Ｔignalsmith-stretch 縺ｫ繧医ｋ繧ｪ繝輔Λ繧､繝ｳ蜃ｦ逅・・
  - TimeStretch: 莨ｸ邵ｮ蛟咲紫・・.25縲・.0・峨〒髟ｷ縺輔ｒ螟画峩縲ゅヴ繝・メ縺ｯ菫晄戟縲Ｔignalsmith-stretch 縺ｫ繧医ｋ繧ｪ繝輔Λ繧､繝ｳ蜃ｦ逅・・
  - Pitch/Stretch 縺ｯ蜃ｦ逅・′驥阪＞蝣ｴ蜷医′縺ゅｋ縺溘ａ縲∝ｮ溯｡御ｸｭ縺ｯ逕ｻ髱｢蜈ｨ菴薙↓繝ｭ繝ｼ繝・ぅ繝ｳ繧ｰ繧ｫ繝舌・繧定｡ｨ遉ｺ縺励※螳御ｺ・ｾ後↓閾ｪ蜍募渚譏・井ｻ悶・驥阪＞蜃ｦ逅・↓繧ゆｽｿ縺・屓縺怜庄閭ｽ縺ｪ蜈ｱ騾壹が繝ｼ繝舌・繝ｬ繧､・峨・
  - UI 縺ｯ縲後そ繧ｰ繝｡繝ｳ繝亥喧縺輔ｌ縺・Mode 蛻・崛 + 蟆丞梛縺ｮ謨ｰ蛟､繧ｹ繝・ャ繝托ｼ・ragValue・峨阪〒邨ｱ荳縲よ枚蟄鈴ｫ倥＆繧呈純縺医∵ｨｪ蟷・頃譛峨ｒ譛蟆丞喧縲・
- 繝ｪ繧ｹ繝亥・: File | Folder | Length | Ch | SR | Bits | dBFS (Peak) | LUFS (I) | Gain(dB) | Wave
  - 蜷・・縺ｯ繝ｪ繧ｵ繧､繧ｺ蜿ｯ閭ｽ縺ｧ縲∝・譛溷ｹ・・譛驕ｩ蛹匁ｸ医∩
  - 髟ｷ縺・ユ繧ｭ繧ｹ繝茨ｼ医ヵ繧｡繧､繝ｫ蜷阪・繝輔か繝ｫ繝繝代せ・峨・閾ｪ蜍募・繧願ｩｰ繧・ｼ・..・芽｡ｨ遉ｺ縲√・繝舌・縺ｧ蜈ｨ譁・｡ｨ遉ｺ
  - Ch/SR/Bits/Length 縺ｯ蜿ｯ隕冶｡瑚｡ｨ遉ｺ譎ゅ↓蜊ｳ繝倥ャ繝諠・ｱ繧定ｪｭ繧薙〒蜿肴丐・磯ｫ倬滂ｼ・
  - dBFS(Peak)/LUFS(I)/Wave 縺ｯ繝舌ャ繧ｯ繧ｰ繝ｩ繧ｦ繝ｳ繝峨〒騾先ｬ｡險育ｮ励＠縺ｦ荳頑嶌縺搾ｼ磯撼蜷梧悄・・
  - dBFS(Peak) 縺ｨ LUFS(I) 縺ｯ蛟､縺ｫ蠢懊§縺ｦ閭梧勹濶ｲ繧堤捩濶ｲ・井ｽ・蟇定牡竊帝ｫ・證冶牡・・
- Gain 蛻励・ dB 縺ｧ邱ｨ髮・庄閭ｽ・・24..+24・峨り､・焚驕ｸ謚樔ｸｭ縺ｫ蟇ｾ雎｡陦後〒隱ｿ謨ｴ縺吶ｋ縺ｨ縲∝､画峩驥上′驕ｸ謚槫・菴薙↓荳諡ｬ驕ｩ逕ｨ縲よ悴菫晏ｭ倥・陦後・繝輔ぃ繧､繝ｫ蜷肴忰蟆ｾ縺ｫ " 窶｢" 繧定｡ｨ遉ｺ
- 繧ｽ繝ｼ繝・ 繝倥ャ繝繧ｯ繝ｪ繝・け縺ｧ縲梧・鬆・・髯埼・・蜈・・鬆・阪ｒ繝医げ繝ｫ・域枚蟄怜・縺ｯUTF鬆・∵焚蛟､縺ｯ螟ｧ蟆城・´ength蛻励・遘呈焚鬆・ｼ・
- 陦後・縺ｩ縺薙〒繧ゅけ繝ｪ繝・け縺ｧ驕ｸ謚橸ｼ矩浹螢ｰ繝ｭ繝ｼ繝峨√ヵ繧｡繧､繝ｫ蜷阪ム繝悶Ν繧ｯ繝ｪ繝・け縺ｧ繧ｿ繝悶ｒ髢九￥縲√ヵ繧ｩ繝ｫ繝蜷阪ム繝悶Ν繧ｯ繝ｪ繝・け縺ｧOS縺ｮ繝輔ぃ繧､繝ｫ繝悶Λ繧ｦ繧ｶ繧帝幕縺擾ｼ郁ｩｲ蠖展AV繧帝∈謚樒憾諷具ｼ・
- 繧ｭ繝ｼ繝懊・繝画桃菴懊〒驕ｸ謚槭＠縺溯｡後・閾ｪ蜍輔〒隕九∴繧倶ｽ咲ｽｮ縺ｸ繧ｹ繧ｯ繝ｭ繝ｼ繝ｫ・医け繝ｪ繝・け驕ｸ謚槭〒縺ｯ菴咲ｽｮ繧堤ｶｭ謖・ｼ・
- 繝ｪ繧ｹ繝医・ Wave 蛻励・ min/max 縺ｮ邁｡譏捺緒逕ｻ縲ゅお繝・ぅ繧ｿ縺ｧ縺ｯ繧ｺ繝ｼ繝/繝代Φ/繧ｷ繝ｼ繧ｯ縺ｫ蟇ｾ蠢懊・
- 豕｢蠖｢陦ｨ遉ｺ縺ｯ Volume 縺ｫ縺ｯ蠖ｱ髻ｿ縺輔ｌ縺ｾ縺帙ｓ・亥ｸｸ縺ｫ 0 dB 縺ｨ隕九↑縺呻ｼ峨・ain(dB) 縺ｮ縺ｿ蜿肴丐縺輔ｌ縺ｾ縺吶・
- 繧ｨ繝・ぅ繧ｿ縺ｮ繝ｫ繝ｼ繝励・荳企Κ繝舌・縺ｧ Off/On/Marker 繧貞・譖ｿ縲√Ν繝ｼ繝礼ｯ・峇邱ｨ髮・・ Inspector > LoopEdit 縺ｫ髮・ｴ・・
- 荳企Κ繝舌・縺ｫ譛ｪ菫晏ｭ倥ご繧､繝ｳ莉ｶ謨ｰ・・Unsaved Gains: N"・峨ｒ陦ｨ遉ｺ
- Export 繝｡繝九Η繝ｼ:
  - Save Selected (Ctrl+E): 驕ｸ謚樔ｸｭ縺ｮ繝輔ぃ繧､繝ｫ縺ｸ繧ｲ繧､繝ｳ繧帝←逕ｨ縺励※菫晏ｭ假ｼ・verwrite・蒐ew File 縺ｯ Settings 縺ｧ謖・ｮ夲ｼ・
  - Apply Gains (new files): 縺吶∋縺ｦ縺ｮ菫晉蕗荳ｭ繧ｲ繧､繝ｳ繧貞酔荳繝輔か繝ｫ繝縺ｫ譁ｰ隕・WAV 縺ｨ縺励※荳諡ｬ蜃ｺ蜉・
  - Clear All Gains: 縺吶∋縺ｦ縺ｮ菫晉蕗荳ｭ繧ｲ繧､繝ｳ繧堤ｴ譽・
  - Settings: 菫晏ｭ伜・繝輔か繝ｫ繝・上ヵ繧｡繧､繝ｫ蜷阪ユ繝ｳ繝励Ξ繝ｼ繝茨ｼ・name}, {gain:+0.0} 縺ｪ縺ｩ・会ｼ剰｡晉ｪ∵凾縺ｮ謖吝虚・・ename/Overwrite/Skip・会ｼ衆verwrite 譎ゅ・ .bak 菴懈・・就ppearance・・ark/Light・・
- **蜀咲函譁ｹ蠑・*:
  - **繝ｪ繧ｹ繝郁｡ｨ遉ｺ譎・*: 蟶ｸ縺ｫ繝ｫ繝ｼ繝礼┌蜉ｹ・井ｸ蠎ｦ蜀咲函縺ｧ蛛懈ｭ｢縲∬ｩｦ閨ｴ縺ｫ譛驕ｩ・・
  - **繧ｨ繝・ぅ繧ｿ陦ｨ遉ｺ譎・*: 繝ｫ繝ｼ繝怜・逕溘・繧ｪ繝ｳ/繧ｪ繝募・譖ｿ蜿ｯ閭ｽ・育┌髻ｳ繧ｮ繝｣繝・・縺ｪ縺励・繧ｷ繝ｼ繝繝ｬ繧ｹ繝ｫ繝ｼ繝暦ｼ・
  - Pitch/Stretch 縺ｮ縺ｨ縺阪・繧｢繝ｫ繧ｴ繝ｪ繧ｺ繝縺ｮ蜃ｺ蜉帙Ξ繧､繝・Φ繧ｷ縺ｨ谿九ｊ蜃ｺ蜉幢ｼ・lush・峨ｒ閠・・縺励※譛ｫ蟆ｾ縺悟・繧後↑縺・ｈ縺・ｪｿ謨ｴ縲ゅΝ繝ｼ繝礼ｶ吶℃逶ｮ縺ｮ蠑輔▲縺九°繧翫ｒ菴取ｸ帙・

蟆・擂・医Ο繝ｼ繝峨・繝・・・・

- 繧ｿ繝悶・縲後・繝・・繧｢繧ｦ繝医搾ｼ晏挨繧ｦ繧｣繝ｳ繝峨え蛹厄ｼ医・繝ｫ繝√え繧｣繝ｳ繝峨え・・
- 繧ｺ繝ｼ繝/繝代Φ縲√す繝ｼ繧ｯ繝舌・縲、窶釘 繝ｫ繝ｼ繝励∵ｳ｢蠖｢繧ｵ繝繝阪う繝ｫ蛻励∬牡縺ｫ繧医ｋ螟ｧ縺ｾ縺九↑髻ｳ驥剰｡ｨ迴ｾ
- 螟壼ｽ｢蠑擾ｼ・p3/ogg/flac/aac・峨→鬮伜刀雉ｪ繝ｪ繧ｵ繝ｳ繝励Ν
- 蜃ｺ蜉帙ョ繝舌う繧ｹ驕ｸ謚槭√ち繧ｰ/繝｡繧ｿ陦ｨ遉ｺ縲√せ繝壹け繝医Ν陦ｨ遉ｺ

---

## 逕ｻ髱｢讒区・

- 荳企Κ繝舌・: 繝輔か繝ｫ繝/繝輔ぃ繧､繝ｫ驕ｸ謚橸ｼ医Γ繝九Η繝ｼ縲靴hoose縲・ Folder... / Files...・峨∫ｷ乗焚陦ｨ遉ｺ縲・浹驥上√Δ繝ｼ繝蛾∈謚橸ｼ・peed/Pitch/Stretch・峨∵､懃ｴ｢繝舌・縲‥BFS 繝｡繝ｼ繧ｿ縲∝・逕溘・繧ｿ繝ｳ・・pace・・
- 繝ｪ繧ｹ繝育判髱｢: File | Folder | Length | Ch | SR | Bits | Level(dBFS) | Wave
  - 蛻励Μ繧ｵ繧､繧ｺ蜿ｯ閭ｽ縲・聞縺・ユ繧ｭ繧ｹ繝医・閾ｪ蜍募・繧願ｩｰ繧・ｼ九・繝舌・陦ｨ遉ｺ縲∽ｻｮ諠ｳ蛹悶せ繧ｯ繝ｭ繝ｼ繝ｫ蟇ｾ蠢・
- 豕｢蠖｢繧ｨ繝・ぅ繧ｿ・医ち繝厄ｼ・ 繝輔Ν豕｢蠖｢縲∝桙逶ｴ繝励Ξ繧､繝倥ャ繝峨√げ繝ｪ繝・ラ邱壹√Ν繝ｼ繝励ヨ繧ｰ繝ｫ・亥・蝓滂ｼ・
  - 繧ｯ繝ｪ繝・け縺ｧ繧ｷ繝ｼ繧ｯ縲√け繝ｪ繝・け&繝峨Λ繝・げ縺ｧ繧ｹ繧ｯ繝ｩ繝・
  - Ctrl+繝帙う繝ｼ繝ｫ縺ｧ譎る俣繧ｺ繝ｼ繝縲ヾhift+繝帙う繝ｼ繝ｫ・医∪縺溘・讓ｪ繝帙う繝ｼ繝ｫ・峨〒蟾ｦ蜿ｳ繝代Φ

蜍穂ｽ懊う繝｡繝ｼ繧ｸ

1) 襍ｷ蜍輔☆繧九→繝ｪ繧ｹ繝育判髱｢縲ゅΓ繝九Η繝ｼ縲靴hoose縲阪°繧・Folder... 縺ｾ縺溘・ Files... 繧帝∈謚槭√ｂ縺励￥縺ｯ繧ｦ繧｣繝ｳ繝峨え縺ｸ繝峨Λ繝・げ&繝峨Ο繝・・縲・
2) 陦後け繝ｪ繝・け竊帝∈謚橸ｼ矩浹螢ｰ繝ｭ繝ｼ繝峨ゅヵ繧｡繧､繝ｫ蜷阪ム繝悶Ν繧ｯ繝ｪ繝・け竊偵お繝・ぅ繧ｿ繧ｿ繝悶ｒ髢九￥・域里蟄倥ち繝悶′縺ゅｌ縺ｰ蜿ｳ蛛ｴ縺ｫ霑ｽ蜉・峨・
3) 繝輔か繝ｫ繝蜷阪ム繝悶Ν繧ｯ繝ｪ繝・け竊丹S 縺ｮ繝輔ぃ繧､繝ｫ繝悶Λ繧ｦ繧ｶ縺ｧ繝輔か繝ｫ繝繧帝幕縺阪仝AV 繧帝∈謚樒憾諷九〒陦ｨ遉ｺ縲・
3) Space 縺ｧ蜀咲函/蛛懈ｭ｢縲ょ・逕滉ｸｭ縺ｯ繝励Ξ繧､繝倥ャ繝峨′遘ｻ蜍輔ゆｸ企Κ縺ｮ dBFS 繝｡繝ｼ繧ｿ縺悟渚譏縺輔ｌ繧九・

---

## 菴ｿ縺・婿 / 繝薙Ν繝・

隕∽ｻｶ: Rust stable縲√が繝ｼ繝・ぅ繧ｪ蜃ｺ蜉帙′譛牙柑縺ｪ Windows/macOS/Linux縲・
PitchShift/TimeStretch・・ignalsmith-stretch・峨ｒ菴ｿ縺・↓縺ｯ C/C++ 繝・・繝ｫ繝√ぉ繝ｼ繝ｳ縺ｨ libclang 縺悟ｿ・ｦ√〒縺吶・

```bash
cargo run
```

### Installer (Windows)

Windows蜷代￠縺ｯ Inno Setup 縺ｧ繧､繝ｳ繧ｹ繝医・繝ｩ繧堤函謌舌＠縺ｾ縺吶・蟆・擂逧・↑繝槭Ν繝√・繝ｩ繝・ヨ繝輔か繝ｼ繝蛹悶ｒ隕区紺縺医※縺・∪縺吶′縲∫樟迥ｶ縺ｯWindows縺ｮ縺ｿ蟇ｾ蠢懊〒縺吶・
Build (Release) + Inno Setup:
```powershell
cargo build --release
"C:\\Program Files (x86)\\Inno Setup 6\\ISCC.exe" installer\\NeoWaves.iss
```

Output:
- `dist\\NeoWaves-Setup-<version>.exe`

Notes:
- 譌｢螳壹・繧､繝ｳ繧ｹ繝医・繝ｫ蜈医・ `C:\\ProgramData\\NeoWaves`
- 繧ｻ繝・ヨ繧｢繝・・繧｢繧､繧ｳ繝ｳ繧剃ｽｿ縺・ｴ蜷医・ `icons\\icon.ico` 繧堤畑諢上＠縺ｦ `SetupIconFile` 繧呈怏蜉ｹ蛹悶＠縺ｦ縺上□縺輔＞

### Automation (CLI)

```bash
cargo run -- --open-folder "C:\\path\\to\\wav" --open-first --screenshot screenshots\\shot.png --exit-after-screenshot
```

Options:
- --open-folder <dir>
- --open-file <wav> (repeatable)
- --open-first
- --open-view-mode <wave|spec|mel>
- --waveform-overlay <on|off>
- --screenshot <path.png>
- --screenshot-delay <frames>
- --exit-after-screenshot
- --dummy-list <count>
- --debug
- --debug-log <path>
- --auto-run
- --auto-run-pitch-shift <semitones>
- --auto-run-time-stretch <rate>
- --auto-run-delay <frames>
- --auto-run-no-exit
- --debug-check-interval <frames>
- F9 saves a screenshot into the OS screenshot folder (Windows: `Pictures\\Screenshots`)
- F12 toggles the debug window

### MCP (stdio/http)

The app exposes a small MCP server over stdio (JSON-RPC, one request per line).
You can start it either from the UI or via CLI flags.

UI:
- Tools -> Start MCP (stdio)
- Tools -> Start MCP (HTTP)
  - If a root folder is already opened, it is used as the default allow-path.

CLI:
- `--mcp-stdio` enables MCP over stdio.
- `--mcp-http` enables MCP over HTTP (`127.0.0.1:7464`).
- `--mcp-http-addr <addr>` set HTTP bind address (example: `127.0.0.1:9000`).
- `--mcp-allow-path <path>` add allowed path (repeatable).
- `--mcp-allow-write` allow write operations.
- `--mcp-allow-export` allow export operations.
- `--mcp-readwrite` disable read-only mode (same as allowing writes).

Example (stdio request):
```
{"jsonrpc":"2.0","id":1,"method":"list_tools","params":{}}
```

Example (HTTP request):
```bash
curl -X POST http://127.0.0.1:7464/rpc -H "Content-Type: application/json" ^
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"list_tools\",\"params\":{}}"
```

MCP Debugging (HTTP):
- Set `NEOWAVES_MCP_DEBUG=1` before launch to print MCP request logs.
- Run the smoke test script: `powershell -ExecutionPolicy Bypass -File .\\commands\\mcp_smoke.ps1`
  - Optional: `-Addr 127.0.0.1:7464 -ToolName get_debug_summary`

Example:
```bash
cargo run -- --open-folder "C:\path\to\wav" --auto-run --debug-log debug\log.txt
```

襍ｷ蜍募ｾ後∝ｷｦ荳翫・繝｡繝九Η繝ｼ縲靴hoose縲阪°繧・Folder... / Files... 繧帝∈謚槭＠縺ｦ荳隕ｧ繧堤ｽｮ縺肴鋤縺医∪縺吶ゅえ繧｣繝ｳ繝峨え縺ｸ縺ｮ繝峨Λ繝・げ&繝峨Ο繝・・縺ｧ繧りｿｽ蜉蜿ｯ閭ｽ縺ｧ縺吶ゅヵ繧｡繧､繝ｫ蜷阪ｒ繝繝悶Ν繧ｯ繝ｪ繝・け縺励※繧ｿ繝悶〒髢九″縲ヾpace 縺ｧ蜀咲函/蛛懈ｭ｢縲・浹驥上せ繝ｩ繧､繝縺ｨ Mode 縺ｮ謨ｰ蛟､・・peed/Pitch/Stretch・峨〒隱ｿ謨ｴ縲∵､懃ｴ｢繝舌・縺ｧ邨槭ｊ霎ｼ縺ｿ縲・

`wgpu` 縺ｧ蜍穂ｽ懊＠縺ｪ縺・腸蠅・〒縺ｯ縲～eframe` 縺ｮ feature 繧・`glow` 縺ｫ螟画峩縺励※繝薙Ν繝峨＠縺ｦ縺上□縺輔＞縲・

### Windows・・ignalsmith-stretch 繧剃ｽｿ縺・ｴ蜷茨ｼ・
- LLVM 繧偵う繝ｳ繧ｹ繝医・繝ｫ・・ibclang 繧貞性繧・・
  - winget: `winget install -e --id LLVM.LLVM`
- 迺ｰ蠅・､画焚繧定ｨｭ螳夲ｼ・owerShell・・
  - 荳譎・ `$Env:LIBCLANG_PATH = 'C:\\Program Files\\LLVM\\bin\\libclang.dll'`
  - 菴ｵ縺帙※: `$Env:CLANG_PATH = 'C:\\Program Files\\LLVM\\bin\\clang.exe'`
- 蠢・ｦ√↓蠢懊§縺ｦ MSVC C++ Build Tools・・indows SDK 蜷ｫ繧・峨ｒ蟆主・
  - `winget install -e --id Microsoft.VisualStudio.2022.BuildTools`

macOS/Linux 縺ｮ萓・
- macOS: `brew install llvm` 竊・`export LIBCLANG_PATH="$(brew --prefix)/opt/llvm/lib"`
- Ubuntu: `sudo apt-get install llvm-dev libclang-dev clang` 竊・`export LIBCLANG_PATH=/usr/lib/llvm-XX/lib`

---

## 螳溯｣・Γ繝｢・郁ｦ∫せ・・

- 蜃ｺ蜉帙せ繝医Μ繝ｼ繝縺ｯ CPAL 縺ｧ蟶ｸ譎りｵｷ蜍輔ゅΟ繝・け繝輔Μ繝ｼ蜈ｱ譛臥憾諷具ｼ・ArcSwapOption<Vec<f32>>` 縺ｨ `Atomic*`・峨↓繝舌ャ繝輔ぃ/蜀咲函菴咲ｽｮ/髻ｳ驥・RMS/繝ｫ繝ｼ繝鈴伜沺繧剃ｿ晄戟縲・
- WAV 縺ｯ `hound` 縺ｧ隱ｭ縺ｿ霎ｼ縺ｿ縲√Δ繝弱Λ繝ｫ蛹悶＠縺ｦ邁｡譏薙Μ繧ｵ繝ｳ繝励Ν・育ｷ壼ｽ｢・峨・
- 豕｢蠖｢陦ｨ遉ｺ縺ｯ蝗ｺ螳壹ン繝ｳ縺ｮ min/max 繧剃ｺ句燕險育ｮ励＠縺ｦ謠冗判縲・
- 繧ｿ繝・UI 縺ｯ `egui` 縺ｮ繧ｿ繝・繧ｳ繝ｳ繝・リ縺ｧ螳溯｣・ょｰ・擂 `egui` 縺ｮ繝槭Ν繝√ン繝･繝ｼ繝昴・繝医〒繝昴ャ繝励い繧ｦ繝亥ｯｾ蠢應ｺ亥ｮ壹・
- 隕冶ｦ・ 繝繝ｼ繧ｯ/繝ｩ繧､繝亥・譖ｿ・・ettings > Appearance・峨ゅョ繝輔か繝ｫ繝医・繝繝ｼ繧ｯ縲よ律譛ｬ隱槭ヵ繧ｩ繝ｳ繝茨ｼ・eiryo/Yu Gothic/MSGothic 遲会ｼ峨ｒ OS 縺九ｉ蜍慕噪隱ｭ縺ｿ霎ｼ縺ｿ・・indows・峨・
- 繧ｹ繝繝ｼ繧ｺ縺ｪ蜀肴緒逕ｻ: 60fps 逶ｮ螳峨〒 `request_repaint_after(16ms)` 繧剃ｽｿ逕ｨ縲・

### 繝｢繧ｸ繝･繝ｼ繝ｫ讒区・

- `src/audio.rs`: 蜀咲函繧ｨ繝ｳ繧ｸ繝ｳ・・PAL・峨→蜈ｱ譛臥憾諷九ゅす繝ｼ繝繝ｬ繧ｹ繝ｫ繝ｼ繝・髻ｳ驥・繝｡繝ｼ繧ｿ/蜀咲函騾溷ｺｦ・育ｷ壼ｽ｢陬憺俣・峨・
- `src/wave.rs`: 繝・さ繝ｼ繝峨・繝ｪ繧ｵ繝ｳ繝励Ν繝ｻ豕｢蠖｢(min/max)菴懈・縺ｨ貅門ｙ繝倥Ν繝代１itch/Stretch 逕ｨ縺ｫ `signalsmith-stretch` 繧剃ｽｿ逕ｨ縺励◆繧ｪ繝輔Λ繧､繝ｳ蜃ｦ逅・ｼ亥・蜉帙Ξ繧､繝・Φ繧ｷ/flush 繧定・・・峨・
- `src/app/`・・UI・・
  - `app.rs`: egui 繧｢繝励Μ譛ｬ菴難ｼ・pdate 繝ｫ繝ｼ繝励√ン繝･繝ｼ讒狗ｯ会ｼ峨・
  - `types.rs`: App 蜀・Κ縺ｮ蝙具ｼ・EditorTab`/`FileMeta`/`RateMode`/`SortKey` 縺ｪ縺ｩ・峨・
  - `helpers.rs`: UI 繝倥Ν繝托ｼ・B竊疲険蟷・濶ｲ縲√・繝・ム繧ｽ繝ｼ繝医√ヵ繧ｩ繝ｼ繝槭ャ繝医＾S 騾｣謳ｺ・峨・
  - `meta.rs`: 繝｡繧ｿ縺ｮ繝舌ャ繧ｯ繧ｰ繝ｩ繧ｦ繝ｳ繝臥函謌舌Ρ繝ｼ繧ｫ繝ｼ・・MS/繧ｵ繝繝搾ｼ峨・
  - `logic.rs`: 髱・UI 繝ｭ繧ｸ繝・け・郁ｵｰ譟ｻ/讀懃ｴ｢/繧ｽ繝ｼ繝・D&D 繝槭・繧ｸ/驥榊・逅・ｵｷ蜍包ｼ峨・
- 驥阪＞蜃ｦ逅・ｼ・itch/Stretch 遲会ｼ峨・蛻･繧ｹ繝ｬ繝・ラ縺ｧ螳溯｡後＠縲ゞI 縺ｯ蜈ｨ逕ｻ髱｢繝ｭ繝ｼ繝・ぅ繝ｳ繧ｰ繧ｪ繝ｼ繝舌・繝ｬ繧､縺ｧ蜈･蜉帙ｒ繝悶Ο繝・け縲ょｮ御ｺ・凾縺ｫ邨先棡・域ｳ｢蠖｢/繝舌ャ繝輔ぃ・峨ｒ驕ｩ逕ｨ縲・

邱ｨ髮・ｩ溯・縺ｮ莉墓ｧ倥・ `docs/EDITOR_SPEC.md` 繧貞盾辣ｧ縺励※縺上□縺輔＞縲・
- `src/main.rs`: 繧ｨ繝ｳ繝医Μ繝昴う繝ｳ繝医・

---

## 繝医Λ繝悶Ν繧ｷ繝･繝ｼ繝・ぅ繝ｳ繧ｰ

- No default output device: OS 蛛ｴ縺ｧ譛牙柑縺ｪ蜃ｺ蜉帙ョ繝舌う繧ｹ繧定ｨｭ螳・
- Unsupported sample format: 迴ｾ蝨ｨ縺ｯ `f32` 蜃ｺ蜉帛燕謠舌ょｿ・ｦ√↓蠢懊§縺ｦ螟画鋤繧呈検繧
- GUI 縺瑚ｵｷ蜍輔＠縺ｪ縺・ `wgpu` 竊・`glow` 縺ｸ蛻・崛繧呈､懆ｨ・
- 繧ｨ繝・ぅ繧ｿ縺ｮ譎る俣繧ｺ繝ｼ繝/繝代Φ縺御ｸ榊ｮ牙ｮ壹↑蝣ｴ蜷医′縺ゅｊ縺ｾ縺吶よ里遏･縺ｮ蝗樣∩遲悶・ `docs/KNOWN_ISSUES.md` 繧貞盾辣ｧ縺励※縺上□縺輔＞縲・

---

## 繝ｭ繝ｼ繝峨・繝・・ / Next

- Speed 繝ｩ繝吶Ν縺ｮ繝励Ν繝繧ｦ繝ｳ蛹厄ｼ・peed / PitchShift / TimeStretch 縺ｮ繝｢繝ｼ繝蛾∈謚橸ｼ・
- ・亥ｮ溯｣・ｸ茨ｼ峨Μ繧ｹ繝医・ dBFS・・ain・臥ｷｨ髮・→菫晏ｭ倥ゆｻ雁ｾ後・ Normalize/LUFS 蟇ｾ蠢懊ｒ讀懆ｨ・
- 繧ｨ繝・ぅ繧ｿ讖溯・縺ｮ諡｡蜈・ｼ郁ｨ育判荳ｭ・夐撼遐ｴ螢顔ｷｨ髮・ｼ・
  - 豕｢蠖｢: 繝医Μ繝溘Φ繧ｰ縲∝燕蠕後ヵ繧ｧ繝ｼ繝峨∝燕蠕後け繝ｭ繧ｹ繝輔ぉ繝ｼ繝峨√Ν繝ｼ繝励・繝ｼ繧ｫ繝ｼ・九Ν繝ｼ繝怜｢・阜繧ｯ繝ｭ繧ｹ繝輔ぉ繝ｼ繝・
  - 繧ｹ繝壹け繝医Ο繧ｰ繝ｩ繝: 驕ｸ謚槭ヮ繧､繧ｺ髯､蜴ｻ縲∝捉豕｢謨ｰ譁ｹ蜷代・逕ｻ蜒冗噪繝ｯ繝ｼ繝・
  - 繝｡繝ｫ繧ｹ繝壹け繝医Ο繧ｰ繝ｩ繝: 髢ｲ隕ｧ縺ｮ縺ｿ・亥・譛滓ｮｵ髫趣ｼ・
  - WORLD 迚ｹ蠕ｴ驥・ F0 繧ｵ繝ｳ繝励Ν繝ｬ繝吶Ν邱ｨ髮・√せ繝壹け繝医Ν蛹・ｵ｡縺ｮ蜻ｨ豕｢謨ｰ譁ｹ蜷代Ρ繝ｼ繝・
  - UI 讎りｦ・ 繝医ャ繝励ヰ繝ｼ荳九↓邱ｨ髮・ち繝悶√◎縺ｮ荳九↓邱ｨ髮・さ繝ｳ繝医Ο繝ｼ繝ｫ縲√＆繧峨↓荳九↓豕｢蠖｢/繧ｹ繝壹け繝医Ο繧ｰ繝ｩ繝遲峨ｒ邵ｦ遨阪∩・域凾髢楢ｻｸ蜈ｱ譛会ｼ・
- 繧ｨ繝・ぅ繧ｿ縺ｮ豕｢蠖｢繧偵メ繝｣繝ｳ繝阪Ν縺斐→縺ｫ蛻・牡陦ｨ遉ｺ
- 螟壼ｽ｢蠑擾ｼ・p3/ogg/flac/aac・峨→鬮伜刀雉ｪ繝ｪ繧ｵ繝ｳ繝励Ν・・symphonia` 莠亥ｮ夲ｼ・
- 蜃ｺ蜉帙ョ繝舌う繧ｹ驕ｸ謚槭√ち繧ｰ/繝｡繧ｿ陦ｨ遉ｺ

隧ｳ邏ｰ縺ｯ `docs/EDITOR_SPEC.md` 縺ｮ縲窪diting Roadmap (Planned)縲阪ｒ蜿ら・縲・

---

## 雋｢迪ｮ

- `rustfmt` / `clippy`・・cargo clippy -- -D warnings`・・
- 蟆上＆縺ｪ PR 豁楢ｿ弱ょ・迴ｾ謇矩・→蜍穂ｽ懃｢ｺ隱阪ｒ譏手ｨ倥＠縺ｦ縺上□縺輔＞

---

## 繝ｩ繧､繧ｻ繝ｳ繧ｹ / 繧ｯ繝ｬ繧ｸ繝・ヨ

TBD・・IT / Apache-2.0 繧呈Φ螳夲ｼ峨ょ推繝ｩ繧､繝悶Λ繝ｪ縺ｮ闡嶺ｽ懈ｨｩ縺ｯ縺昴ｌ縺槭ｌ縺ｮ繝励Ο繧ｸ繧ｧ繧ｯ繝医↓蟶ｰ螻槭＠縺ｾ縺吶・

---

## FAQ

- 繧ｷ繝ｧ繝ｼ繝医き繝・ヨ: Space・亥・逕・蛛懈ｭ｢・峨゜/P・医Ν繝ｼ繝鈴幕蟋・邨ゆｺ・ｒ繝励Ξ繧､繝倥ャ繝峨°繧芽ｨｭ螳夲ｼ峨´・医Ν繝ｼ繝・On/Off・峨ヾ・医ぞ繝ｭ繧ｯ繝ｭ繧ｹ繧ｹ繝翫ャ繝怜・譖ｿ・峨，trl+S・磯∈謚樔ｿ晏ｭ假ｼ峨，trl+W・医ち繝悶ｒ髢峨§繧具ｼ峨，trl+A・亥・驕ｸ謚橸ｼ峨≫・/竊難ｼ磯∈謚樒ｧｻ蜍包ｼ峨ヾhift+竊・竊難ｼ育ｯ・峇驕ｸ謚橸ｼ峨≫・/竊抵ｼ・ain 隱ｿ謨ｴ: 譌｢螳・ﾂｱ0.1dB / Shift ﾂｱ1.0 / Ctrl ﾂｱ3.0・峨・nter・医お繝・ぅ繧ｿ繧帝幕縺擾ｼ・
- WAV 莉･螟・ 莉翫・髱槫ｯｾ蠢懊Ａsymphonia` 邨・∩霎ｼ縺ｿ蠕後↓諡｡蠑ｵ

---

Maintainers: 蛻晄悄險ｭ險・@you・医ワ繝ｳ繝峨が繝墓ｸ茨ｼ峨ょｼ輔″邯吶℃繝｡繝ｳ繝舌・縺ｯ霑ｽ險倥＠縺ｦ縺上□縺輔＞縲・

