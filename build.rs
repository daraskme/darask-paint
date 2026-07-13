//! Windows 実行ファイルへアイコン(`assets/icon.ico`)を埋め込む
//! (SPEC §29「exe アイコン」、ARCHITECTURE.md §16.8)。
//!
//! v4 の落とし穴 #7(ARCHITECTURE.md §16.10-7)「winresource は Windows
//! 以外では何もしないよう build.rs を組む」への対応: darask-paint 自体は
//! Windows 専用(CLAUDE.md)で CI も windows-latest のみだが、他環境での
//! コンパイルエラーの種を残さないよう `target_os` で丸ごとガードする。

fn main() {
    #[cfg(target_os = "windows")]
    {
        let icon_path = "assets/icon.ico";
        println!("cargo:rerun-if-changed={icon_path}");

        let mut res = winresource::WindowsResource::new();
        res.set_icon(icon_path);
        // ビルドスクリプトの失敗はビルド自体を止める(このアプリの
        // 「I/O・ユーザー入力経路で unwrap/panic しない」という品質基準は
        // 実行時のアプリ本体に対するものであり、ビルド時にのみ動く
        // build.rs には適用されない — アイコン埋め込みに失敗した状態で
        // 気づかずリリースする方が害が大きい)。
        if let Err(e) = res.compile() {
            panic!("{icon_path} の exe への埋め込みに失敗しました: {e}");
        }
    }
}
