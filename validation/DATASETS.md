# 水泥磨測試資料調查

調查日期：2026-10-25。範圍：Hugging Face dataset API/card，以及相關工業論文與資料作者的公開來源。

## 結論

**在已檢查的 Hugging Face 候選中，尚未確認可用於水泥磨 MPC 閉迴路驗證的真實動態時序資料。** 這不是聲稱整個平台絕對沒有此類資料。不要把水泥價格、混凝土強度、影像或水泥窯資料當成水泥磨歷史。

沒有下載/執行資料集提供的程式，也沒有把第三方原始資料納入本 repository。`generate_fixture.py` 產生的資料是明確標記的合成線性系統，僅供軟體 smoke/regression 使用，不是水泥磨模型。

## Hugging Face 候選

| 來源 | 觀察 | 適用性 |
|---|---|---|
| [cement mill 搜尋 API](https://huggingface.co/api/datasets?search=cement%20mill&limit=20)、[cement kiln 搜尋 API](https://huggingface.co/api/datasets?search=cement%20kiln&limit=20) | 本次查詢返回空清單 | 僅能證明這些搜尋詞未命中 |
| [MuhammadAhmadSaaim/CementDataset](https://huggingface.co/datasets/MuhammadAhmadSaaim/CementDataset) | card 顯示 305 筆 Date/Price | 價格，不是製程 |
| [JYJ233/cement200](https://huggingface.co/datasets/JYJ233/cement200) | 200 筆 audio/sentence | 音訊文字，不是製程 |
| [SITU-YH/Cement](https://huggingface.co/datasets/SITU-YH/Cement) | imagefolder/image modality | 影像，不是時序 |
| [electricsheepafrica/african-cement-plant-operations](https://huggingface.co/datasets/electricsheepafrica/african-cement-plant-operations) | card 為 CC BY 4.0、月度多廠情境/catalog 資料；非已驗證的高頻量測 | 不適合估計磨機動態、死區或控制效果 |
| [Soumojit048/grinding-zarr-datasets](https://huggingface.co/datasets/Soumojit048/grinding-zarr-datasets) | 機器人研磨示範 pipeline；[API](https://huggingface.co/api/datasets/Soumojit048/grinding-zarr-datasets) 授權 unknown | 不是水泥研磨 |

## 公開來源的替代候選

### 水泥窯：可研究，不能替代水泥磨

[YSUzzp/CementCalcinationProcessData](https://github.com/YSUzzp/CementCalcinationProcessData) 的 [README](https://raw.githubusercontent.com/YSUzzp/CementCalcinationProcessData/main/README.txt) 宣稱河北水泥產線真實回轉窯、非平穩操作、1 分鐘平均、20 個製程變數/指標。

- [CSV metadata](https://api.github.com/repos/YSUzzp/CementCalcinationProcessData/contents/CementCalcinationProcessData.csv)：17,692,412 bytes。
- 先前讀取計得 80,883 行（含表頭）；GBK 解碼表頭可讀。首筆 `202403100000` 與 README 宣稱的 `2024-03-01` 起日不一致。尚未完成整份時間戳連續性/缺值稽核。
- 包含 `f_CaO`、分解爐煤回饋、喂料量回饋、多處溫度/壓力、風機電流、窯電流、能耗等。
- **電流或回饋量不等於可操縱設定值**。沒有 SP/Mode、量測延遲與單位說明時，不能自動指定 MV，亦不能把 f_CaO 缺測或零值當可靠連續分析儀資料。
- [LICENSE](https://raw.githubusercontent.com/YSUzzp/CementCalcinationProcessData/main/LICENSE) 是 Boost Software License 1.0；README 未明確說明 CSV 授權範圍。使用/再散布前應確認資料本身的權利、引用方式與匿名化授權。

用途：取得上述確認後可測試匯入及水泥窯的觀察性模型擬合。不能宣稱是水泥磨模型的獨立測試集。

### 水泥磨論文：有方法與模型，尚無已確認可直接下載的歷史

- [Tsamatsoulis 2014, Halyps CM5](https://www.scielo.br/j/bjce/a/TswqLVDtt6L4vhbx8ZwbSGg/?lang=en)，[DOI](https://doi.org/10.1590/S0104-66322014000100015)：描述 1 分鐘取樣、250 分鐘連續操作片段，以及磨機喂料、回料提升機功率/選粉機回料流量的控制關係。文章本身未提供已確認可下載的原始歷史。
- [Sivanandam et al. 2024](https://pmc.ncbi.nlm.nih.gov/articles/PMC11682061/)，[DOI](https://doi.org/10.1038/s41598-024-82708-w)：步階來自 FLSmidth 模擬器，3% 喂料變動、提升機電流響應，並辨識 FOPDT。圖形不是原始工廠時序；文章 CC BY-NC-ND 4.0 亦不能不經檢查便當作任意資料再製授權。

## 真實資料的准入條件

1. 確認設備是目標水泥磨（球磨/立磨及流程），不是窯或靜態品質表。
2. 取得時間戳、取樣週期、時區、單位、品質碼、啟停/故障/模式與產品切換資訊。
3. MV 必須對應部署時實際操縱位置；區分 PID SP、PV、OP，不能互換單位。
4. CV 需有量測延遲，尤其實驗室細度/Blaine/殘餘篩分；DV 需確認在評估範圍內不受 MPC 操作顯著影響。
5. 確認輸入有足夠激勵，沒有完全共線；未知交互影響不能直接填 0。
6. 按時間/完整工況切分 train、validation、test；品質不良與不連續區段不得靜默插值。
7. 授權、來源與檔案雜湊隨模型報告保存。真實工廠資料留在本機，不送外部模型供應者。

在上述條件滿足以前，只能得到軟體流程證據，不能得到「MPC 符合真實水泥磨」的結論。
