//! 画面レイアウトを担当するモジュール群(SPEC §3 の画面構成)。
//!
//! M1 でレイアウトの枠組みを作り、M2/M3/M4 で各モジュールに実際の機能
//! (ツール切り替え・色選択・ズーム操作・ファイル I/O・選択・画像メニュー等)
//! を配線した。`dialogs` は M4 で追加した、新規/画像サイズ変更/
//! キャンバスサイズ変更/JPEG 品質/未保存確認のモーダル群。

pub mod color_panel;
pub mod color_wheel;
pub mod dialogs;
pub mod icons;
pub mod layers_panel;
pub mod menu;
pub mod options_bar;
pub mod side_panel;
pub mod status_bar;
pub mod toolbar;
