# IMB-C 固定樣本（複製品）

來源：IMB-C 原始碼樹的 `tests/fixtures/`（<https://github.com/Kaido-MSVC/IMB-C>，IMB-C v1.3.0，Copyright (c) 2026 Kaido-MSVC，MIT）。依 IMB-C 的 `tests/fixtures/acceptance-v13/README.md`，這些都是本機產生的合成小圖，不含使用者照片或個人 metadata。

複製日期：2026-11-07。只讀取、複製，未寫入 IMB-C。複製前後原檔與副本的 SHA-256 均已核對一致。

| 檔案 | 原始位置 | bytes | SHA-256 | 用途 |
|---|---|---|---|---|
| v13-icc16.png | tests/fixtures/v13-icc16.png | 2285 | `328b81ab392f29ef6c43f9415b76b670e6cbb34c1416f287f9ee35056fdeaefe` | 16-bit RGB + iCCP |
| v13-Rec2100PQ.png | tests/fixtures/v13-Rec2100PQ.png | 5257 | `075065e01bf6784756f50a977282e762d06127c27556be03d10a4239558bec99` | 16-bit PQ，iCCP + cHRM |
| v13-Rec2100HLG.png | tests/fixtures/v13-Rec2100HLG.png | 6628 | `bf86f5f878892f5bf3b663aa2c8dc831073f4f5611ce184067531195c4cfef8f` | 16-bit HLG，iCCP + cHRM |
| v13-cicp-only-pq.png | tests/fixtures/v13-cicp-only-pq.png | 3049 | `5e698c41fa7f2fbdb562156206889cc9ff99d1595c02c0cf826a233f3acbe7b6` | 只有 cICP（BT.2020/PQ） |
| v13-hidden.png | tests/fixtures/v13-hidden.png | 78 | `efee920b803934766468df302c6428f3121619f1d006cadb05d6306ce8a43030` | 全透明像素的隱藏 RGB |
| invalid-idat-v121.png | tests/fixtures/invalid-idat-v121.png | 74 | `fb412b2743fa6602826492b3e60c5fff9751bce7a72986ab417495363bb1c6ee` | 損壞的 IDAT |
| icc8.png | tests/fixtures/acceptance-v13/icc8.png | 5231 | `5743b65f862181a2824c16dcea78e7cbd2b7d0faed4b6edfc0070fb4a9a4ee0f` | 8-bit RGB + iCCP + cHRM |
| pq-original.jxl | tests/fixtures/acceptance-v13/pq-original.jxl | 112 | `c1c38c24e7233bc8410eaeee44dc64768149d54a6296a59832043a7d4bc6cb00` | libjxl 0.12.0 產生的 16-bit PQ |
| hlg-original.jxl | tests/fixtures/acceptance-v13/hlg-original.jxl | 112 | `137162745628fe8ef089ff60bf37503e51b1c8403fbeb903b9fb4a3187ea3bae` | libjxl 0.12.0 產生的 16-bit HLG |
| animation.webp | tests/fixtures/animation.webp | 204 | `e6cd271073c4ec538d875eb2ba3c34eed8681e3a00a216b911da84f8681c421f` | 動畫，應拒絕嚴格判定 |
| multipage-v12.tiff | tests/fixtures/multipage-v12.tiff | 1872 | `1ec71ea31e9e89b8623ff291b47965b16eadb9dc7ff72fb264bb2f39ae3b4c19` | 多頁，應拒絕嚴格判定 |
| rgba-v12.png | tests/fixtures/rgba-v12.png | 932 | `0666e1b7896285bf29fc83dfdf99f19c6c076abd29d9e17ce0e23312bbe39caf` | 階段 3 轉檔輸入（#6、#7） |
| gray-v12.png | tests/fixtures/gray-v12.png | 299 | `1d2aeb1c1bfffb33000f27002a2c47582491c72dff5a376af029335a1777b76d` | 階段 3 轉檔輸入（#8） |
| rgb-v12.png | tests/fixtures/rgb-v12.png | 744 | `a03b0161f56932a7d72258bb633267da3f6395aa800689ef24d9e58e2325341d` | 階段 3 轉檔輸入（#9、#10） |

## IMB-C v1.3.0 GUI 轉檔輸出（階段 3）

由使用者以 IMB-C v1.3.0 GUI 把上表的合成小圖轉出到本機暫存資料夾，2026-11-07 複製到此。輸入全部是上表的 IMB-C 固定樣本，沒有使用私人圖片。只讀取、複製；複製前後來源檔與副本的 SHA-256 均已核對一致。

輸出設定：
- PNG、JXL、TIFF：IMB-C v1.3.0 只有無損輸出，沒有可設定的選項。
- WebP：IMB-C v1.3.0 的 WebP 有「無損」開關，沒有其他已知選項。各檔的開關狀態**由位元流推定，不是轉檔時記錄的設定**：#5、#6、#14 是 VP8L，推定為無損開啟；#9 是 VP8，推定為無損關閉。
- #11（v13-icc16.png → WebP 真無損）：IMB-C 拒絕轉出，沒有輸出檔。這是預期行為（WebP 只有 8-bit，16-bit 原圖無法真正無損）。拒絕訊息原文：

  > 失敗: <IMB-C>\tests\fixtures\v13-icc16.png — WebP 僅支援 8-bit；原圖高於 8-bit，無法真正無損。請選 PNG/TIFF/JXL，或關閉 WebP 無損以允許降低位深

| # | 副本檔名 | 轉出時的檔名 | 輸入 | bytes | SHA-256 | 備註 |
|---|---|---|---|---|---|---|
| 1 | v13-icc16__jxl.jxl | 同左 | v13-icc16.png | 2158 | `7ea4a4ea3a17f19e555d89b6df71294cc77ba6b029550d203babef427b886141` | |
| 2 | v13-Rec2100PQ__jxl.jxl | v13-Rec2100HLG__jxl.jxl | v13-icc16.png 或 v13-Rec2100PQ.png | 2158 | `7ea4a4ea3a17f19e555d89b6df71294cc77ba6b029550d203babef427b886141` | 原檔名有誤（內含 PQ ICC `745265d1…`）。使用者不記得是從哪個檔轉的；兩個輸入的樣本與 ICC 相同，無法區分。測試上當作 #2。與 #1、#4 逐 byte 相同 |
| 3 | v13-Rec2100HLG__jxl.jxl | v13-Rec2100HLG.jxl | v13-Rec2100HLG.png | 2855 | `8cc7c4e393e9b97fadafb415a0170bf6234fe918f1361cba3fcfdcf6e9cb1f1c` | 原檔名少了 `__jxl`；內含 HLG ICC `8c50cf64…` |
| 4 | v13-cicp-only-pq__jxl.jxl | 同左 | v13-cicp-only-pq.png | 2158 | `7ea4a4ea3a17f19e555d89b6df71294cc77ba6b029550d203babef427b886141` | 輸入只有 cICP，輸出改嵌入 PQ ICC |
| 5 | v13-hidden__webp.webp | 同左 | v13-hidden.png | 58 | `4a0fff8617f37db7a467c7ad8681e981ac86887b1b1362864f64c858cab30bb0` | VP8L |
| 6 | rgba-v12__webp.webp | 同左 | rgba-v12.png | 86 | `721830cf09dea34aacf34d433fd8cf137762ebc2cd11498364b3ef442446bfba` | VP8L |
| 7 | rgba-v12__jxl.jxl | 同左 | rgba-v12.png | 174 | `8d9eff96c0745b83d01caaca98ef2f40d7d8e3339d3111a1fc7d604cba8dcdba` | |
| 8 | gray-v12__jxl.jxl | 同左 | gray-v12.png | 68 | `841354e9c4619381d5305e372a2376eb3f7d423df4eb511f56c749d02bf381bf` | |
| 9 | rgb-v12__webp-lossy.webp | rgb-v12.webp | rgb-v12.png | 290 | `bbf7aab1a2e00dae574862d726fdb88be6d03a81495f055be02e51c4b115d00b` | VP8 有損；原檔名缺 `__webp-lossy` |
| 10 | rgb-v12__jxl.jxl | rgb-v12__jxl-lossy.jxl | rgb-v12.png | 145 | `b630c8a41f666ad8353caa8dc98277d778e0b92d711e83c62830d37a1c739b63` | IMB-C 沒有 JXL 有損模式，這是一般的無損輸出，清單命名有誤，已去掉 `-lossy`。XYB 有損只由自建測試圖覆蓋 |
| 12 | pq-original__png.png | 同左 | acceptance-v13/pq-original.jxl | 5257 | `656aa4fb62417086deb0dce27b25be47d3673fcc7107b0cd85ab9b5417349746` | |
| 13 | hlg-original__png.png | 同左 | acceptance-v13/hlg-original.jxl | 6628 | `7ed302961c9c48732aa122cc9c9366a2d35a77b6464d1de4d22d241256aa8e9e` | |
| 14 | icc8__webp.webp | icc8__webp.webp.webp | acceptance-v13/icc8.png | 4410 | `cb2b16e0ad5c8d49dc3755f7b6af321d5c76405dcd025e482db41bd1cb0b91e6` | 原檔名的副檔名重複；VP8L＋ICCP |
