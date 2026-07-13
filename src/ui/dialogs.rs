//! モーダルダイアログ(SPEC §7: 新規/画像サイズ変更/キャンバスサイズ変更、
//! SPEC §8: JPEG 品質、SPEC §8: 未保存確認)。
//!
//! `egui::Modal` を使う(SPEC §7: 「ダイアログは egui のモーダル(`egui::Modal`
//! が使えるバージョンならそれを使用)」)。状態(幅・高さ等)は `app.rs` の
//! `ModalState` が保持し、ここでは `&mut` で受け取って描画するだけの薄い層に
//! する。戻り値の `DialogOutcome`/`ConfirmOutcome` で「まだ開いている」
//! 「確定」「キャンセル」を呼び出し側(app.rs)に伝える。

use eframe::egui;

use crate::document::{Background, Interpolation};

/// OK/キャンセルの 2 択ダイアログ共通の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogOutcome {
    /// まだ開いている(ユーザーがまだ確定/キャンセルしていない)。
    Pending,
    Confirmed,
    Cancelled,
}

/// 未保存確認ダイアログ(SPEC §8)の結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmOutcome {
    Pending,
    Save,
    Discard,
    Cancel,
}

/// 「新規」ダイアログ(SPEC §7: 幅・高さ(1-8192、デフォルト 1280×720)、
/// 背景 = 白/透明)。
pub fn show_new(
    ctx: &egui::Context,
    width: &mut u32,
    height: &mut u32,
    background: &mut Background,
) -> DialogOutcome {
    let mut outcome = DialogOutcome::Pending;
    let modal = egui::Modal::new(egui::Id::new("darask_dialog_new")).show(ctx, |ui| {
        ui.heading("新規作成");
        size_fields(ui, width, height);
        ui.horizontal(|ui| {
            ui.label("背景:");
            ui.radio_value(background, Background::White, "白");
            ui.radio_value(background, Background::Transparent, "透明");
        });
        ui.separator();
        confirm_buttons(ui, &mut outcome);
    });
    if modal.should_close() && outcome == DialogOutcome::Pending {
        outcome = DialogOutcome::Cancelled;
    }
    outcome
}

/// 「画像サイズ変更」ダイアログ(SPEC §7: 縦横比固定チェック(デフォルト
/// ON)、補間 = バイリニア/ニアレスト)。`orig_width`/`orig_height` は縦横比
/// 固定の計算に使う現在のドキュメントサイズ。
pub fn show_image_resize(
    ctx: &egui::Context,
    width: &mut u32,
    height: &mut u32,
    keep_aspect: &mut bool,
    interpolation: &mut Interpolation,
    orig_width: u32,
    orig_height: u32,
) -> DialogOutcome {
    let mut outcome = DialogOutcome::Pending;
    let modal = egui::Modal::new(egui::Id::new("darask_dialog_image_resize")).show(ctx, |ui| {
        ui.heading("画像サイズ変更");
        ui.horizontal(|ui| {
            ui.label("幅:");
            let mut w = *width;
            if ui
                .add(egui::DragValue::new(&mut w).range(1..=8192))
                .changed()
            {
                *width = w.clamp(1, 8192);
                if *keep_aspect && orig_width > 0 {
                    *height = ((*width as f32) * (orig_height as f32 / orig_width as f32))
                        .round()
                        .clamp(1.0, 8192.0) as u32;
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("高さ:");
            let mut h = *height;
            if ui
                .add(egui::DragValue::new(&mut h).range(1..=8192))
                .changed()
            {
                *height = h.clamp(1, 8192);
                if *keep_aspect && orig_height > 0 {
                    *width = ((*height as f32) * (orig_width as f32 / orig_height as f32))
                        .round()
                        .clamp(1.0, 8192.0) as u32;
                }
            }
        });
        ui.checkbox(keep_aspect, "縦横比を固定");
        ui.horizontal(|ui| {
            ui.label("補間:");
            ui.radio_value(interpolation, Interpolation::Bilinear, "バイリニア");
            ui.radio_value(interpolation, Interpolation::Nearest, "ニアレスト");
        });
        ui.separator();
        confirm_buttons(ui, &mut outcome);
    });
    if modal.should_close() && outcome == DialogOutcome::Pending {
        outcome = DialogOutcome::Cancelled;
    }
    outcome
}

/// 「キャンバスサイズ変更」ダイアログ(SPEC §7: 既存画像は左上基準で配置、
/// 拡張部分は透明)。
pub fn show_canvas_resize(ctx: &egui::Context, width: &mut u32, height: &mut u32) -> DialogOutcome {
    let mut outcome = DialogOutcome::Pending;
    let modal = egui::Modal::new(egui::Id::new("darask_dialog_canvas_resize")).show(ctx, |ui| {
        ui.heading("キャンバスサイズ変更");
        ui.weak("既存の画像は左上基準で配置され、拡張された部分は透明になります。");
        size_fields(ui, width, height);
        ui.separator();
        confirm_buttons(ui, &mut outcome);
    });
    if modal.should_close() && outcome == DialogOutcome::Pending {
        outcome = DialogOutcome::Cancelled;
    }
    outcome
}

/// JPEG 品質ダイアログ(SPEC §8: 1-100、デフォルト 90)。
pub fn show_jpeg_quality(ctx: &egui::Context, quality: &mut u8) -> DialogOutcome {
    let mut outcome = DialogOutcome::Pending;
    let modal = egui::Modal::new(egui::Id::new("darask_dialog_jpeg_quality")).show(ctx, |ui| {
        ui.heading("JPEG 品質");
        let mut q = *quality as i32;
        if ui.add(egui::Slider::new(&mut q, 1..=100)).changed() {
            *quality = q.clamp(1, 100) as u8;
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("保存").clicked() {
                outcome = DialogOutcome::Confirmed;
            }
            if ui.button("キャンセル").clicked() {
                outcome = DialogOutcome::Cancelled;
            }
        });
    });
    if modal.should_close() && outcome == DialogOutcome::Pending {
        outcome = DialogOutcome::Cancelled;
    }
    outcome
}

/// SPEC §24: 「明るさ・コントラスト…」(各 -100〜+100、ライブプレビュー)。
/// 戻り値の `bool` は「このフレームでスライダーの値が変わったか」
/// (ARCHITECTURE.md §14.9-8 と同じ「値が変わったフレームだけ再適用」方式。
/// 呼び出し側はこれが `true` のときだけ `app.rs::reapply_tone_preview` を呼ぶ)。
pub fn show_brightness_contrast(
    ctx: &egui::Context,
    brightness: &mut i32,
    contrast: &mut i32,
) -> (DialogOutcome, bool) {
    let mut outcome = DialogOutcome::Pending;
    let mut changed = false;
    let modal =
        egui::Modal::new(egui::Id::new("darask_dialog_brightness_contrast")).show(ctx, |ui| {
            ui.heading("明るさ・コントラスト");
            ui.horizontal(|ui| {
                ui.label("明るさ:");
                if ui.add(egui::Slider::new(brightness, -100..=100)).changed() {
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("コントラスト:");
                if ui.add(egui::Slider::new(contrast, -100..=100)).changed() {
                    changed = true;
                }
            });
            ui.separator();
            confirm_buttons(ui, &mut outcome);
        });
    if modal.should_close() && outcome == DialogOutcome::Pending {
        outcome = DialogOutcome::Cancelled;
    }
    (outcome, changed)
}

/// SPEC §24: 「色相・彩度・明度…」(色相 -180〜+180、彩度/明度 -100〜+100、
/// ライブプレビュー)。戻り値は `show_brightness_contrast` と同じ規則。
pub fn show_hue_saturation(
    ctx: &egui::Context,
    hue: &mut i32,
    saturation: &mut i32,
    lightness: &mut i32,
) -> (DialogOutcome, bool) {
    let mut outcome = DialogOutcome::Pending;
    let mut changed = false;
    let modal = egui::Modal::new(egui::Id::new("darask_dialog_hue_saturation")).show(ctx, |ui| {
        ui.heading("色相・彩度・明度");
        ui.horizontal(|ui| {
            ui.label("色相:");
            if ui.add(egui::Slider::new(hue, -180..=180)).changed() {
                changed = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("彩度:");
            if ui.add(egui::Slider::new(saturation, -100..=100)).changed() {
                changed = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("明度:");
            if ui.add(egui::Slider::new(lightness, -100..=100)).changed() {
                changed = true;
            }
        });
        ui.separator();
        confirm_buttons(ui, &mut outcome);
    });
    if modal.should_close() && outcome == DialogOutcome::Pending {
        outcome = DialogOutcome::Cancelled;
    }
    (outcome, changed)
}

/// 未保存変更ガード(SPEC §8: 「保存しますか?」保存/破棄/キャンセル)。
/// `doc_label` はタイトルバーと同じ「ファイル名」表記(無題なら「無題」)。
pub fn show_confirm_unsaved(ctx: &egui::Context, doc_label: &str) -> ConfirmOutcome {
    let mut outcome = ConfirmOutcome::Pending;
    let modal = egui::Modal::new(egui::Id::new("darask_dialog_confirm_unsaved")).show(ctx, |ui| {
        ui.heading("保存しますか?");
        ui.label(format!("「{doc_label}」への変更を保存しますか?"));
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("保存").clicked() {
                outcome = ConfirmOutcome::Save;
            }
            if ui.button("破棄").clicked() {
                outcome = ConfirmOutcome::Discard;
            }
            if ui.button("キャンセル").clicked() {
                outcome = ConfirmOutcome::Cancel;
            }
        });
    });
    if modal.should_close() && outcome == ConfirmOutcome::Pending {
        outcome = ConfirmOutcome::Cancel;
    }
    outcome
}

/// v4 §26: 「ヘルプ > バージョン情報」。版数・リポジトリ URL を表示するだけの
/// 小モーダル(閉じるだけなので `DialogOutcome::Cancelled` は使わず、
/// `Confirmed` 1 本で「閉じた」を表す)。
pub fn show_about(ctx: &egui::Context, version: &str, repository: &str) -> DialogOutcome {
    let mut outcome = DialogOutcome::Pending;
    let modal = egui::Modal::new(egui::Id::new("darask_dialog_about")).show(ctx, |ui| {
        ui.heading("Darask Paint");
        ui.label(format!("バージョン: {version}"));
        ui.hyperlink_to(repository, repository);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("閉じる").clicked() {
                outcome = DialogOutcome::Confirmed;
            }
        });
    });
    if modal.should_close() && outcome == DialogOutcome::Pending {
        outcome = DialogOutcome::Confirmed;
    }
    outcome
}

fn size_fields(ui: &mut egui::Ui, width: &mut u32, height: &mut u32) {
    ui.horizontal(|ui| {
        ui.label("幅:");
        let mut w = *width;
        if ui
            .add(egui::DragValue::new(&mut w).range(1..=8192))
            .changed()
        {
            *width = w.clamp(1, 8192);
        }
    });
    ui.horizontal(|ui| {
        ui.label("高さ:");
        let mut h = *height;
        if ui
            .add(egui::DragValue::new(&mut h).range(1..=8192))
            .changed()
        {
            *height = h.clamp(1, 8192);
        }
    });
}

fn confirm_buttons(ui: &mut egui::Ui, outcome: &mut DialogOutcome) {
    ui.horizontal(|ui| {
        if ui.button("OK").clicked() {
            *outcome = DialogOutcome::Confirmed;
        }
        if ui.button("キャンセル").clicked() {
            *outcome = DialogOutcome::Cancelled;
        }
    });
}
