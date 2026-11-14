//! Player-facing text, Thai and English.

pub const PREFIX: &str = "&8[&bLogin&8]&r ";

/// Message for a key. Unknown keys come back as the key itself so a typo shows up in a test.
pub fn t(thai: bool, key: &'static str) -> &'static str {
    let (th, en) = match key {
        "ask-login" => ("&eพิมพ์ &f/login <รหัสผ่าน> &eเพื่อเข้าเล่น", "&eType &f/login <password> &eto play"),
        "ask-register" => ("&eพิมพ์ &f/register <รหัสผ่าน> <รหัสผ่านอีกครั้ง> &eเพื่อสมัครก่อนเข้าเล่น", "&eType &f/register <password> <password again> &eto sign up"),
        "ask-register-1" => ("&eพิมพ์ &f/register <รหัสผ่าน> &eเพื่อสมัครก่อนเข้าเล่น", "&eType &f/register <password> &eto sign up"),
        "title-login" => ("&e/login <รหัสผ่าน>", "&e/login <password>"),
        "title-register" => ("&e/register <รหัสผ่าน>", "&e/register <password>"),
        "bar-left" => ("&7เหลือเวลา &f{secs} &7วินาที", "&f{secs} &7seconds left"),
        "logged-in" => ("&aเข้าสู่ระบบแล้ว", "&aLogged in"),
        "session" => ("&aเข้าสู่ระบบอัตโนมัติจากครั้งก่อน", "&aLogged in from your last visit"),
        "registered" => ("&aสมัครเรียบร้อย ครั้งหน้าใช้ &f/login <รหัสผ่าน>", "&aSigned up. Next time use &f/login <password>"),
        "optional" => ("&7เซิร์ฟนี้ตั้งรหัสผ่านกันคนอื่นใช้ชื่อคุณได้ด้วย &f/register <รหัสผ่าน> <รหัสผ่านอีกครั้ง>", "&7You can protect your name with &f/register <password> <password again>"),
        "wrong" => ("&cรหัสผ่านไม่ถูกต้อง เหลืออีก {left} ครั้ง", "&cWrong password, {left} tries left"),
        "wrong-old" => ("&cรหัสผ่านเดิมไม่ถูกต้อง", "&cThe current password is wrong"),
        "already-in" => ("&7คุณเข้าสู่ระบบอยู่แล้ว", "&7You are already logged in"),
        "already-registered" => ("&cชื่อนี้สมัครไว้แล้ว ใช้ &f/login <รหัสผ่าน>", "&cThis name is registered, use &f/login <password>"),
        "not-registered" => ("&cยังไม่ได้สมัคร ใช้ &f/register", "&cNot signed up yet, use &f/register"),
        "login-first" => ("&cเข้าสู่ระบบก่อนจึงจะทำสิ่งนี้ได้", "&cLog in first"),
        "usage-login" => ("&eวิธีใช้: &f/login <รหัสผ่าน>", "&eUsage: &f/login <password>"),
        "usage-register" => ("&eวิธีใช้: &f/register <รหัสผ่าน> <รหัสผ่านอีกครั้ง>", "&eUsage: &f/register <password> <password again>"),
        "usage-register-1" => ("&eวิธีใช้: &f/register <รหัสผ่าน>", "&eUsage: &f/register <password>"),
        "usage-change" => ("&eวิธีใช้: &f/changepassword <รหัสผ่านเดิม> <รหัสผ่านใหม่>", "&eUsage: &f/changepassword <current> <new>"),
        "usage-unregister" => ("&eวิธีใช้: &f/unregister <รหัสผ่าน>", "&eUsage: &f/unregister <password>"),
        "mismatch" => ("&cรหัสผ่านสองช่องไม่ตรงกัน", "&cThe two passwords do not match"),
        "pw-short" => ("&cรหัสผ่านสั้นเกินไป ต้องมีอย่างน้อย {min} ตัว", "&cPassword too short, at least {min} characters"),
        "pw-long" => ("&cรหัสผ่านยาวเกินไป", "&cPassword too long"),
        "pw-chars" => ("&cรหัสผ่านห้ามมีช่องว่าง", "&cPasswords cannot contain spaces"),
        "pw-name" => ("&cห้ามใช้ชื่อตัวเองเป็นรหัสผ่าน", "&cDo not use your name as the password"),
        "pw-weak" => ("&cรหัสผ่านนี้เดาง่ายเกินไป ลองใช้คำอื่น", "&cThat password is too easy to guess"),
        "ip-limit" => ("&cเครื่องนี้สมัครครบจำนวนบัญชีที่อนุญาตแล้ว", "&cToo many accounts from this address"),
        "slow" => ("&7ช้าลงหน่อย ลองใหม่ในอีกสักครู่", "&7Slow down and try again in a moment"),
        "busy" => ("&7มีคนเข้าสู่ระบบพร้อมกันหลายคน ลองใหม่ในอีกสักครู่", "&7The server is busy, try again in a moment"),
        "failed" => ("&cทำรายการไม่สำเร็จ ลองใหม่อีกครั้ง", "&cThat did not work, try again"),
        "changed" => ("&aเปลี่ยนรหัสผ่านแล้ว", "&aPassword changed"),
        "logged-out" => ("&7ออกจากระบบแล้ว", "&7Logged out"),
        "unregistered" => ("&7ลบบัญชีแล้ว", "&7Account removed"),
        "ingame-only" => ("&cคำสั่งนี้ใช้ได้เฉพาะในเกม", "&cThis command only works in game"),
        "retry" => ("&7ลองพิมพ์คำสั่งอีกครั้ง", "&7Try the command again"),
        "inactive" => ("&7Login ไม่ทำงาน เพราะเซิร์ฟนี้เปิด online mode (Mojang ยืนยันตัวผู้เล่นให้แล้ว)", "&7Login is idle: this server is in online mode (Mojang already verifies players)"),
        // Kick screens: plain text
        "kick-timeout" => ("เข้าสู่ระบบไม่ทันเวลา เข้ามาใหม่แล้วลองอีกครั้ง", "Login timed out. Join again"),
        "kick-tries" => ("ใส่รหัสผ่านผิดหลายครั้งเกินไป", "Too many wrong passwords"),
        "kick-locked" => ("ใส่รหัสผ่านของชื่อนี้ผิดหลายครั้งเกินไป ลองใหม่ในอีก {mins} นาที", "Too many wrong passwords for this name. Try again in {mins} minutes"),
        "kick-name" => ("ชื่อผู้เล่นต้องยาว 3-16 ตัว ใช้ได้เฉพาะ A-Z a-z 0-9 และ _", "Names must be 3-16 characters of A-Z a-z 0-9 and _"),
        "kick-case" => ("ชื่อนี้สมัครไว้เป็น {name} ให้เข้าด้วยชื่อที่สะกดแบบนั้น", "This name is registered as {name}. Join with exactly that spelling"),
        "kick-online" => ("มีผู้เล่นชื่อนี้ออนไลน์อยู่แล้ว", "A player with this name is already online"),
        "kick-removed" => ("บัญชีของคุณถูกลบโดยผู้ดูแล", "Your account was removed by an admin"),
        // Admin
        "a-help" => (
            "&eLogin: &f/auth info <ชื่อ> &7| &fregister <ชื่อ> <รหัสผ่าน> &7| &fsetpassword <ชื่อ> <รหัสผ่าน> &7| &funregister <ชื่อ> &7| &fforcelogin <ชื่อ> &7| &funlock <ชื่อ|all> &7| &fimport <ไฟล์> &7| &fpurge <วัน> &7| &fstatus &7| &freload &7| &fversion",
            "&eLogin: &f/auth info <name> &7| &fregister <name> <password> &7| &fsetpassword <name> <password> &7| &funregister <name> &7| &fforcelogin <name> &7| &funlock <name|all> &7| &fimport <file> &7| &fpurge <days> &7| &fstatus &7| &freload &7| &fversion",
        ),
        "a-reloaded" => ("&aโหลดไฟล์ตั้งค่าใหม่แล้ว", "&aConfiguration reloaded"),
        "a-none" => ("&cไม่มีบัญชีชื่อ {name}", "&cNo account named {name}"),
        "a-exists" => ("&cมีบัญชีชื่อ {name} อยู่แล้ว", "&cAn account named {name} already exists"),
        "a-info" => ("&f{name} &7สมัครเมื่อ {reg} จาก {regip} · เข้าล่าสุด {last} จาก {lastip} · ตอนนี้ {state}", "&f{name} &7registered {reg} from {regip} · last login {last} from {lastip} · now {state}"),
        "a-set" => ("&aตั้งรหัสผ่านของ {name} แล้ว", "&aPassword of {name} set"),
        "a-created" => ("&aสร้างบัญชี {name} แล้ว", "&aAccount {name} created"),
        "a-removed" => ("&aลบบัญชี {name} แล้ว", "&aAccount {name} removed"),
        "a-forced" => ("&aให้ {name} เข้าสู่ระบบแล้ว", "&a{name} is now logged in"),
        "a-not-waiting" => ("&7{name} ไม่ได้รอเข้าสู่ระบบอยู่", "&7{name} is not waiting to log in"),
        "a-unlocked" => ("&aปลดล็อกแล้ว {n} รายการ", "&aUnlocked {n}"),
        "a-purged" => ("&aลบบัญชีที่ไม่ได้เข้านานกว่า {days} วัน {n} บัญชี", "&aRemoved {n} accounts unused for more than {days} days"),
        "a-purge-usage" => ("&eวิธีใช้: &f/auth purge <วัน> &7(อย่างน้อย 30)", "&eUsage: &f/auth purge <days> &7(30 or more)"),
        "a-status" => ("&7บัญชี &f{accounts} &7· กำลังรอเข้าสู่ระบบ &f{waiting} &7· เข้าสู่ระบบแล้ว &f{in} &7· ถูกล็อก &f{locked} &7· session &f{sessions}", "&7accounts &f{accounts} &7· waiting &f{waiting} &7· logged in &f{in} &7· locked &f{locked} &7· sessions &f{sessions}"),
        "a-import-usage" => ("&eวิธีใช้: วางไฟล์ไว้ใน plugins/data/login/ แล้วสั่ง &f/auth import <ชื่อไฟล์> &7(บรรทัดละ ชื่อ:hash แบบ $SHA$ ของ AuthMe)", "&ePut the file into plugins/data/login/ then &f/auth import <file name> &7(one name:hash per line, AuthMe $SHA$ hashes)"),
        "a-import-missing" => ("&cอ่านไฟล์ {name} ใน plugins/data/login/ ไม่ได้", "&cCannot read {name} in plugins/data/login/"),
        "a-imported" => ("&aนำเข้า {n} บัญชี &7(ข้าม {skipped} บรรทัดที่ชื่อไม่ถูกหรือ hash ไม่ใช่แบบ $SHA$ และข้ามชื่อที่มีอยู่แล้ว)", "&aImported {n} accounts &7({skipped} lines skipped: bad name or not a $SHA$ hash; existing names are kept)"),
        "a-bad-name" => ("&cชื่อนี้ใช้ไม่ได้", "&cThat name is not valid"),
        "s-waiting" => ("รอเข้าสู่ระบบ", "waiting to log in"),
        "s-in" => ("เข้าสู่ระบบแล้ว", "logged in"),
        "s-off" => ("ออฟไลน์", "offline"),
        "never" => ("ยังไม่เคย", "never"),
        "ago" => ("{t} ที่แล้ว", "{t} ago"),
        other => (other, other),
    };
    if thai { th } else { en }
}

/// `3 วัน` / `5 ชม.` / `12 นาที`, coarse on purpose.
pub fn span(thai: bool, secs: u64) -> String {
    let (n, th, en) = match secs {
        s if s >= 86_400 => (s / 86_400, "วัน", "d"),
        s if s >= 3_600 => (s / 3_600, "ชม.", "h"),
        s if s >= 60 => (s / 60, "นาที", "min"),
        s => (s, "วินาที", "s"),
    };
    format!("{n} {}", if thai { th } else { en })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_has_both_languages() {
        for key in [
            "ask-login", "ask-register", "ask-register-1", "title-login", "title-register", "bar-left", "logged-in", "session",
            "registered", "optional", "wrong", "wrong-old", "already-in", "already-registered", "not-registered", "login-first",
            "usage-login", "usage-register", "usage-register-1", "usage-change", "usage-unregister", "mismatch", "pw-short",
            "pw-long", "pw-chars", "pw-name", "pw-weak", "ip-limit", "slow", "busy", "failed", "changed", "logged-out",
            "unregistered", "ingame-only", "retry", "inactive", "kick-timeout", "kick-tries", "kick-locked", "kick-name",
            "kick-case", "kick-online", "kick-removed", "a-help", "a-reloaded", "a-none", "a-exists", "a-info", "a-set",
            "a-created", "a-removed", "a-forced", "a-not-waiting", "a-unlocked", "a-purged", "a-purge-usage", "a-status",
"a-bad-name", "a-import-usage", "a-import-missing", "a-imported", "s-waiting", "s-in", "s-off", "never", "ago",
        ] {
            // Thai text never equals its key; English may (the word "never")
            assert_ne!(t(true, key), key, "{key}");
            assert!(!t(false, key).is_empty(), "{key}");
            assert!(!t(true, key).contains('—') && !t(false, key).contains('—'));
        }
        assert_eq!(t(true, "nope"), "nope");
        assert_eq!(span(true, 200_000), "2 วัน");
        assert_eq!(span(false, 59), "59 s");
    }
}
