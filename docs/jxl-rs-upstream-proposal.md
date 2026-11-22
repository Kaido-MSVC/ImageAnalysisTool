# 給 jxl-rs 上游的修改提案（草稿，尚未提交）

階段 5（D-R18）整理，由使用者決定是否提交。對象是 https://github.com/libjxl/jxl-rs ，針對 0.8.0。以下英文段落可直接作為 issue 內容；提交前請確認上游的最新版本是否已有對應功能。

需要這兩項的原因：
- iat 要回報「多圖層的靜止影像」（`layered_image`）。目前 jxl-rs 只記錄可見的 frame，所以 `jxl_two_layers_vs_composite_png` 暫列為允許差異。
- iat 要判斷「alpha 深度與色彩深度不同」。目前只能自己解析 ImageMetadata。

---

## Issue: expose per-frame layer information and extra-channel bit depth

### 1. `JxlDecoderOptions::coalescing = false` is accepted but has no effect

In 0.8.0 the `coalescing` field exists in `JxlDecoderOptions` and defaults to `true`, but nothing reads it. With `coalescing = false` a still image made of two zero-duration layers still produces a single `Event::FrameHeader`, and `scanned_frames()` only lists visible frames (`FrameHeader::is_visible()`), so a caller cannot find out that the image has layers.

libjxl's behaviour (0.12.0, `decode.cc`): with coalescing disabled, every frame whose `frame_type` is `kRegularFrame` is reported as its own frame (`is_last_of_still |= !coalescing && frame_type == kRegularFrame`), together with its layer info (crop origin and size, blend mode).

Proposal, in order of preference:

1. Implement `coalescing = false` like libjxl: emit `Event::FrameHeader` for every regular frame, and extend `JxlFrameHeader` with the layer fields libjxl exposes in `JxlLayerInfo`:
   ```rust
   pub struct JxlFrameHeader {
       pub name: String,
       pub duration: Option<f64>,
       pub size: (usize, usize),
       // new
       pub layer: Option<JxlLayerInfo>, // None when coalescing is on
   }
   pub struct JxlLayerInfo {
       pub have_crop: bool,
       pub crop_x0: i32,
       pub crop_y0: i32,
       pub xsize: u32,
       pub ysize: u32,
       pub blend_mode: JxlBlendMode, // Replace, Add, Blend, AlphaWeightedAdd, Mul
       pub blend_source: u32,
       pub save_as_reference: u32,
   }
   ```
2. If (1) is too large: record every frame header in scan mode, not only visible ones. For example, add `VisibleFrameInfo::is_visible` (or a separate `scanned_frame_headers()`), with `frame_type`, crop and blend mode, so that a scan pass can count layers without decoding pixels.

### 2. Extra-channel bit depth is not exposed

`JxlExtraChannel` has `ec_type` and `alpha_associated` only. The internal `ExtraChannelInfo` also carries `bit_depth`, `dim_shift`, `name` and the spot colour, which libjxl returns through `JxlExtraChannelInfo` (`bits_per_sample`, `exponent_bits_per_sample`, `dim_shift`, `name`, `spot_color`, `cfa_channel`). A caller that needs, for example, to reject images whose alpha depth differs from the colour depth currently has to parse the ImageMetadata bit stream itself.

Proposal: add the fields to `JxlExtraChannel`:
```rust
pub struct JxlExtraChannel {
    pub ec_type: ExtraChannel,
    pub alpha_associated: bool,
    // new
    pub bit_depth: JxlBitDepth,
    pub dim_shift: u32,
    pub name: String,
    pub spot_color: Option<[f32; 4]>,
}
```
`#[non_exhaustive]` on these structs would let further fields be added later without a breaking change.

### Context

We use jxl-rs to compare images losslessly against libjxl-based results. With the two items above, the remaining API differences we hit would be closed; sample output, orientation handling (`adjust_orientation = false`), native bit depths through `JxlDataFormat::U16 { bit_depth }` and container handling already match libjxl on our test set.
