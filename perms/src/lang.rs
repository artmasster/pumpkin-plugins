//! Player-facing text, Thai and English (plain Rust).

/// Message prefix of every reply.
pub const P: &str = "&8[&bPerms&8]&r ";

/// (key, Thai, English). `{0}` … `{9}` are filled by `fill`.
const TEXT: &[(&str, &str, &str)] = &[
    ("no_perm", "&cคุณไม่มีสิทธิ์ใช้คำสั่งนี้ &7({0})", "&cYou do not have permission for this &7({0})"),
    ("usage", "&eวิธีใช้: &f/lp {0}", "&eUsage: &f/lp {0}"),
    ("unknown_sub", "&cไม่รู้จักคำสั่งย่อย &f{0} &7· ลอง &f/lp help", "&cUnknown subcommand &f{0} &7· try &f/lp help"),
    ("no_user", "&cไม่พบผู้เล่น &f{0} &7(ต้องเคยเข้าเซิร์ฟเวอร์อย่างน้อยหนึ่งครั้ง)", "&cNo player &f{0} &7(they must have joined at least once)"),
    ("no_group", "&cไม่มีกลุ่ม &f{0}", "&cNo group &f{0}"),
    ("no_track", "&cไม่มี track &f{0}", "&cNo track &f{0}"),
    ("bad_name", "&cชื่อ &f{0} &cใช้ไม่ได้ &7(a-z 0-9 _ - ไม่เกิน 32 ตัว)", "&cThe name &f{0} &cis not allowed &7(a-z 0-9 _ -, at most 32)"),
    ("bad_node", "&cnode &f{0} &cใช้ไม่ได้ &7(ห้ามเว้นวรรค ห้ามขึ้นต้นด้วย - ไม่เกิน 160 ตัว)", "&cThe node &f{0} &cis not allowed &7(no spaces, no leading -, at most 160)"),
    ("bad_context", "&cบริบท &f{0} &cไม่ถูกต้อง &7(เช่น world=nether gamemode=creative)", "&cBad context &f{0} &7(for example world=nether gamemode=creative)"),
    ("bad_duration", "&cเวลา &f{0} &cไม่ถูกต้อง &7(เช่น 30s 10m 2h 7d 1mo)", "&cBad duration &f{0} &7(for example 30s 10m 2h 7d 1mo)"),
    ("bad_number", "&cตัวเลข &f{0} &cไม่ถูกต้อง", "&cBad number &f{0}"),
    ("bad_bool", "&cต้องเป็น true หรือ false ไม่ใช่ &f{0}", "&cExpected true or false, not &f{0}"),
    ("full", "&cเก็บ node ได้ไม่เกิน {0} รายการต่อหนึ่งกลุ่มหรือผู้เล่น", "&cA group or player holds at most {0} nodes"),
    ("too_many", "&cมีได้ไม่เกิน {0} รายการ ลบของเก่าก่อน", "&cThere can be at most {0} of these; remove some first"),
    ("unknown_ctx", "&ePerms ยังไม่รู้จักบริบท &f{0} &eจึงบันทึกไว้เฉย ๆ และ node นี้จะยังไม่มีผล", "&eThe context &f{0} &eis stored, but Perms does not know it, so this node never applies"),
    // guard
    ("guard_node", "&cคุณให้ &f{0} &cไม่ได้ เพราะคุณเองไม่มีสิทธิ์นี้", "&cYou cannot give &f{0}&c: you do not hold it yourself"),
    ("guard_undeny", "&cคุณเอาข้อห้าม &f{0} &cออกไม่ได้ เพราะคุณเองไม่มีสิทธิ์นี้", "&cYou cannot lift the denial of &f{0}&c: you do not hold it yourself"),
    ("guard_group", "&cกลุ่ม &f{0} &cมีน้ำหนักสูงกว่ากลุ่มหลักของคุณ", "&cThe group &f{0} &cis heavier than your own primary group"),
    ("guard_user", "&cผู้เล่น &f{0} &cอยู่กลุ่มที่สูงกว่าคุณ", "&cThe player &f{0} &cis in a heavier group than you"),
    ("guard_weight", "&cตั้งน้ำหนักสูงกว่ากลุ่มหลักของคุณไม่ได้ &7({0})", "&cYou cannot set a weight above your own primary group &7({0})"),
    ("guard_group_node", "&cคุณให้กลุ่ม &f{0} &cไม่ได้ เพราะกลุ่มนี้มี &f{1} &cซึ่งคุณเองไม่มี", "&cYou cannot give the group &f{0}&c: it holds &f{1}&c, which you do not"),
    ("guard_owner", "&cคำสั่งนี้ใช้ได้เฉพาะ console หรือ op ระดับ 4", "&cOnly the console or a level 4 operator may do this"),
    // holders
    ("user_title", "&bผู้เล่น &f{0} &7({1}) {2}", "&bPlayer &f{0} &7({1}) {2}"),
    ("online", "&aออนไลน์", "&aonline"),
    ("offline", "&7ออฟไลน์", "&7offline"),
    ("group_title", "&bกลุ่ม &f{0}{1} &7น้ำหนัก &f{2}", "&bGroup &f{0}{1} &7weight &f{2}"),
    ("line_primary", " &7กลุ่มหลัก: &f{0} &7(น้ำหนัก {1})", " &7Primary group: &f{0} &7(weight {1})"),
    ("line_inherits", " &7สืบทอดจาก: &f{0}", " &7Inherits: &f{0}"),
    ("line_prefix", " &7Prefix: &r{0}&7 · Suffix: &r{1}", " &7Prefix: &r{0}&7 · Suffix: &r{1}"),
    ("line_context", " &7บริบทตอนนี้: &f{0}", " &7Current context: &f{0}"),
    ("line_counts", " &7node ของตัวเอง: &f{0} &7(สิทธิ์ {1}, กลุ่ม {2}, meta {3})", " &7Own nodes: &f{0} &7(permissions {1}, parents {2}, meta {3})"),
    ("line_members", " &7สมาชิก: ผู้เล่น &f{0} &7คน, กลุ่มที่สืบทอด &f{1}", " &7Members: &f{0} &7players, &f{1} &7groups inherit it"),
    ("none", "&7(ไม่มี)", "&7(none)"),
    ("implicit", "&7(โดยปริยาย)", "&7(implicit)"),
    ("more", " &7… และอีก {0} รายการ", " &7… and {0} more"),
    ("perm_title", "&bสิทธิ์ของ &f{0} &7({1} รายการ)", "&bPermissions of &f{0} &7({1})"),
    ("parent_title", "&bกลุ่มของ &f{0}", "&bParents of &f{0}"),
    ("meta_title", "&bMeta ของ &f{0}", "&bMeta of &f{0}"),
    ("set", "&aตั้ง {1} &aให้ &f{0} &aแล้ว", "&aSet {1} &afor &f{0}"),
    ("set_same", "&e{0} มี {1} &eอยู่แล้ว", "&e{0} already has {1}"),
    ("unset", "&aเอา {1} &aออกจาก &f{0} &aแล้ว", "&aRemoved {1} &afrom &f{0}"),
    ("unset_none", "&e{0} ไม่มี &f{1} &eที่ตรงกับบริบทนี้", "&e{0} has no &f{1} &ein that context"),
    ("cleared", "&aล้าง {1} รายการของ &f{0} &aแล้ว", "&aCleared {1} nodes of &f{0}"),
    ("check_own", " &7ตั้งไว้เอง: {0}", " &7Set directly: {0}"),
    ("check_eff", " &7Perms ตอบ: {0} &7จาก &f{1} &7(ตรงกับ &f{2}&7)", " &7Perms says: {0} &7from &f{1} &7(matched &f{2}&7)"),
    ("check_none", " &7Perms ไม่ได้ตั้ง node นี้ไว้ จึงใช้ค่าเริ่มต้นของเซิร์ฟเวอร์ (op และค่าที่ปลั๊กอินลงทะเบียน)", " &7Perms has nothing on this node, so the server default applies (op level and the plugin's registered default)"),
    ("check_final", " &7ผลจริงตอนนี้: {0}", " &7Result right now: {0}"),
    ("check_title", "&bตรวจ &f{1} &bของ &f{0}", "&bCheck &f{1} &bfor &f{0}"),
    ("parent_set", "&aตั้งกลุ่มของ &f{0} &aเป็น &f{1} &aแล้ว", "&aSet the group of &f{0} &ato &f{1}"),
    ("parent_add", "&aเพิ่ม &f{0} &aเข้ากลุ่ม &f{1} &aแล้ว{2}", "&aAdded &f{0} &ato &f{1}{2}"),
    ("parent_remove", "&aเอา &f{0} &aออกจากกลุ่ม &f{1} &aแล้ว", "&aRemoved &f{0} &afrom &f{1}"),
    ("parent_not", "&e{0} ไม่ได้อยู่กลุ่ม &f{1}", "&e{0} is not in &f{1}"),
    ("for_time", " &7เป็นเวลา &f{0}", " &7for &f{0}"),
    ("self_parent", "&cกลุ่มสืบทอดจากตัวเองไม่ได้", "&cA group cannot inherit itself"),
    ("meta_set", "&aตั้ง meta &f{1} &a= &f{2} &aให้ &f{0} &aแล้ว", "&aSet meta &f{1} &a= &f{2} &afor &f{0}"),
    ("meta_unset", "&aเอา meta &f{1} &aออกจาก &f{0} &aแล้ว ({2} รายการ)", "&aRemoved meta &f{1} &afrom &f{0} ({2})"),
    ("bad_meta_key", "&cชื่อ meta &f{0} &cใช้ไม่ได้ &7(ห้ามมีจุดหรือเว้นวรรค)", "&cThe meta key &f{0} &cis not allowed &7(no dots or spaces)"),
    ("prefix_set", "&aตั้ง {1} ลำดับ &f{2} &aเป็น &r{3}&a ให้ &f{0} &aแล้ว", "&aSet {1} &f{2} &ato &r{3}&a for &f{0}"),
    ("prefix_removed", "&aเอา {1} ออกจาก &f{0} &aแล้ว ({2} รายการ)", "&aRemoved {1} from &f{0} ({2})"),
    ("too_long", "&cข้อความยาวเกิน {0} ตัวอักษร", "&cText is longer than {0} characters"),
    ("promoted", "&aเลื่อน &f{0} &aบน track &f{1}&a: &f{2} &7→ &f{3}", "&aPromoted &f{0} &aon track &f{1}&a: &f{2} &7→ &f{3}"),
    ("demoted", "&aลด &f{0} &aบน track &f{1}&a: &f{2} &7→ &f{3}", "&aDemoted &f{0} &aon track &f{1}&a: &f{2} &7→ &f{3}"),
    ("track_joined", "&aใส่ &f{0} &aในกลุ่มแรกของ track &f{1}&a: &f{2}", "&aPut &f{0} &ain the first group of track &f{1}&a: &f{2}"),
    ("track_left", "&aเอา &f{0} &aออกจากกลุ่มแรกของ track &f{1} &a(&f{2}&a) แล้ว", "&aTook &f{0} &aoff the first group of track &f{1} &a(&f{2}&a)"),
    ("track_end", "&e{0} อยู่กลุ่มสูงสุดของ track &f{1} &eแล้ว", "&e{0} is already at the end of track &f{1}"),
    ("track_not_on", "&e{0} ไม่ได้อยู่ในกลุ่มใดของ track &f{1}", "&e{0} is in no group of track &f{1}"),
    ("track_ambiguous", "&c{0} อยู่หลายกลุ่มของ track &f{1} &cจึงเลื่อนไม่ได้ เอาออกให้เหลือกลุ่มเดียวก่อน", "&c{0} is in several groups of track &f{1}&c; leave one before moving"),
    ("track_empty", "&ctrack &f{0} &cยังไม่มีกลุ่ม", "&cThe track &f{0} &chas no groups"),
    ("track_missing", "&ctrack &f{0} &cมีกลุ่ม &f{1} &cที่ไม่มีอยู่แล้ว", "&cThe track &f{0} &cnames the group &f{1}&c, which does not exist"),
    ("cloned", "&aคัดลอกทุกอย่างของ &f{0} &aไปให้ &f{1} &aแล้ว ({2} รายการ)", "&aCopied everything of &f{0} &ato &f{1} ({2} nodes)"),
    ("showtracks", "&bTrack ที่ &f{0} &bอยู่: &f{1}", "&bTracks of &f{0}&b: &f{1}"),
    // groups
    ("group_exists", "&cมีกลุ่ม &f{0} &cอยู่แล้ว", "&cThe group &f{0} &calready exists"),
    ("group_created", "&aสร้างกลุ่ม &f{0} &aแล้ว", "&aCreated the group &f{0}"),
    ("group_deleted", "&aลบกลุ่ม &f{0} &aแล้ว (ถอดออกจากผู้เล่น กลุ่มอื่น และ track ด้วย)", "&aDeleted the group &f{0} &a(also taken off players, groups and tracks)"),
    ("group_default", "&cกลุ่ม &f{0} &cเป็นกลุ่มเริ่มต้น ลบหรือเปลี่ยนชื่อไม่ได้", "&cThe group &f{0} &cis the default group and cannot be deleted or renamed"),
    ("group_renamed", "&aเปลี่ยนชื่อกลุ่ม &f{0} &aเป็น &f{1} &aแล้ว", "&aRenamed the group &f{0} &ato &f{1}"),
    ("group_cloned", "&aคัดลอกกลุ่ม &f{0} &aเป็น &f{1} &aแล้ว", "&aCloned the group &f{0} &aas &f{1}"),
    ("weight_set", "&aตั้งน้ำหนักของ &f{0} &aเป็น &f{1} &aแล้ว", "&aSet the weight of &f{0} &ato &f{1}"),
    ("display_set", "&aตั้งชื่อแสดงของ &f{0} &aเป็น &f{1} &aแล้ว", "&aSet the display name of &f{0} &ato &f{1}"),
    ("groups_title", "&bกลุ่มทั้งหมด &7({0})", "&bGroups &7({0})"),
    ("members_title", "&bสมาชิกของ &f{0}", "&bMembers of &f{0}"),
    ("members_users", " &7ผู้เล่น ({0}): &f{1}", " &7Players ({0}): &f{1}"),
    ("members_groups", " &7กลุ่ม ({0}): &f{1}", " &7Groups ({0}): &f{1}"),
    // tracks
    ("track_exists", "&cมี track &f{0} &cอยู่แล้ว", "&cThe track &f{0} &calready exists"),
    ("track_created", "&aสร้าง track &f{0} &aแล้ว", "&aCreated the track &f{0}"),
    ("track_deleted", "&aลบ track &f{0} &aแล้ว", "&aDeleted the track &f{0}"),
    ("tracks_title", "&bTrack ทั้งหมด &7({0})", "&bTracks &7({0})"),
    ("track_info", "&bTrack &f{0}&b: &f{1}", "&bTrack &f{0}&b: &f{1}"),
    ("track_changed", "&atrack &f{0} &aตอนนี้: &f{1}", "&aTrack &f{0} &ais now: &f{1}"),
    ("track_has", "&etrack &f{0} &eมีกลุ่ม &f{1} &eอยู่แล้ว", "&eThe track &f{0} &ealready has &f{1}"),
    ("track_hasnt", "&etrack &f{0} &eไม่มีกลุ่ม &f{1}", "&eThe track &f{0} &edoes not have &f{1}"),
    ("track_renamed", "&aเปลี่ยนชื่อ track &f{0} &aเป็น &f{1} &aแล้ว", "&aRenamed the track &f{0} &ato &f{1}"),
    ("track_cloned", "&aคัดลอก track &f{0} &aเป็น &f{1} &aแล้ว", "&aCloned the track &f{0} &aas &f{1}"),
    ("track_long", "&ctrack มีได้ไม่เกิน 64 กลุ่ม", "&cA track holds at most 64 groups"),
    // general
    ("info_title", "&bPerms &f{0} &7· ระบบสิทธิ์ของ MCSV สำหรับ Pumpkin", "&bPerms &f{0} &7· permissions by MCSV for Pumpkin"),
    ("info_counts", " &7กลุ่ม &f{0} &7· ผู้เล่นที่มีข้อมูล &f{1} &7· track &f{2} &7· ชื่อที่รู้จัก &f{3}", " &7Groups &f{0} &7· players with data &f{1} &7· tracks &f{2} &7· known names &f{3}"),
    ("info_runtime", " &7ผู้เล่นออนไลน์ที่คำนวณไว้ &f{0} &7· ตรวจสิทธิ์ &f{1} &7ครั้ง (Perms ตอบ &f{2}&7) · verbose &f{3}", " &7Online players resolved &f{0} &7· checks &f{1} &7(answered by Perms &f{2}&7) · verbose listeners &f{3}"),
    ("info_resend", " &7ส่งรายการคำสั่งให้ client ใหม่ &f{0} &7ครั้ง เฉลี่ย &f{1} &7ms ต่อครั้ง", " &7Command list sent again &f{0} &7times, &f{1} &7ms each on average"),
    ("info_files", " &7ไฟล์: &fplugins/data/perms/ &7(groups/, users/, tracks.toml, config.toml) แก้แล้ว &f/lp sync", " &7Files: &fplugins/data/perms/ &7(groups/, users/, tracks.toml, config.toml); edit, then &f/lp sync"),
    ("synced", "&aโหลดข้อมูลจากไฟล์ใหม่แล้ว: กลุ่ม &f{0} &aผู้เล่น &f{1} &atrack &f{2}", "&aReloaded from disk: &f{0} &agroups, &f{1} &aplayers, &f{2} &atracks"),
    ("sync_bad", "&eอ่านไฟล์นี้ไม่ได้ จึงข้าม: &f{0}", "&eCould not read this file, skipped: &f{0}"),
    ("tree_title", "&bnode ที่รู้จัก &7({0}){1}", "&bKnown nodes &7({0}){1}"),
    ("tree_scope", " &7ขึ้นต้นด้วย &f{0}", " &7starting with &f{0}"),
    ("tree_hint", " &7รายการนี้มาจาก node ที่ถูกตรวจตั้งแต่เปิดเซิร์ฟเวอร์และ node ในข้อมูล (Pumpkin ไม่มีรายการ node ให้ปลั๊กอินอ่าน)", " &7This list is the nodes checked since the server started plus the nodes in the data (Pumpkin gives plugins no node list)"),
    ("search_title", "&bผู้ที่มี &f{0}&b: {1} รายการ", "&bHolders of &f{0}&b: {1}"),
    ("search_line", " &7- {0} &f{1}&7: {2}", " &7- {0} &f{1}&7: {2}"),
    ("word_user", "ผู้เล่น", "player"),
    ("word_group", "กลุ่ม", "group"),
    ("word_prefix", "prefix", "prefix"),
    ("word_suffix", "suffix", "suffix"),
    ("bad_file", "&cชื่อไฟล์ &f{0} &cใช้ไม่ได้ &7(ตัวอักษร ตัวเลข _ - ไม่เกิน 40 ตัว)", "&cThe file name &f{0} &cis not allowed &7(letters, digits, _ -, at most 40)"),
    ("exported", "&aส่งออกแล้ว: &fplugins/data/perms/exports/{0} &7(กลุ่ม {1}, ผู้เล่น {2}, track {3})", "&aExported: &fplugins/data/perms/exports/{0} &7({1} groups, {2} players, {3} tracks)"),
    ("export_fail", "&cเขียนไฟล์ไม่ได้: &f{0}", "&cCould not write the file: &f{0}"),
    ("import_missing", "&cไม่พบไฟล์ &fplugins/data/perms/exports/{0}", "&cNo file &fplugins/data/perms/exports/{0}"),
    ("import_big", "&cไฟล์ใหญ่เกิน 16 MB", "&cThe file is larger than 16 MB"),
    ("import_bad", "&cอ่านไฟล์ไม่ได้: &f{0}", "&cCould not read the file: &f{0}"),
    ("imported", "&aนำเข้าแล้ว: กลุ่ม &f{0} &aผู้เล่น &f{1} &atrack &f{2}{3}", "&aImported: &f{0} &agroups, &f{1} &aplayers, &f{2} &atracks{3}"),
    ("import_replaced", " &7(แทนที่ข้อมูลเดิมทั้งหมด)", " &7(replaced everything that was here)"),
    ("verbose_on", "&aเปิด verbose แล้ว{0} &7· ปิดด้วย &f/lp verbose off", "&aVerbose is on{0} &7· turn it off with &f/lp verbose off"),
    ("verbose_filter", " &7กรอง: &f{0}", " &7filter: &f{0}"),
    ("verbose_off", "&aปิด verbose แล้ว", "&aVerbose is off"),
    ("editor_start", "&7กำลังเตรียมหน้าเว็บสำหรับแก้สิทธิ์…", "&7Preparing the web editor…"),
    ("editor_link", "&aเปิดลิงก์นี้เพื่อแก้สิทธิ์: &f{0} &7· ลิงก์ใช้ได้ 24 ชั่วโมง ใครมีลิงก์ก็เห็นข้อมูลสิทธิ์ได้ อย่าแชร์ · แก้เสร็จกด บันทึก แล้วนำคำสั่งที่หน้าเว็บให้มาพิมพ์ที่นี่{1}", "&aOpen this link to edit: &f{0} &7· it works for 24 hours and anyone who has it can read the permission data, so do not share it · when done press Save and run the command the page gives you here{1}"),
    ("editor_left", " &e(ผู้เล่น {0} คนไม่ได้อยู่ในหน้าเว็บเพราะข้อมูลใหญ่เกิน แก้คนเหล่านั้นด้วยคำสั่ง /lp user)", " &e({0} players are not on the page because the data is too large; edit those with /lp user)"),
    ("editor_off", "&eเซิร์ฟนี้ยังไม่ได้ตั้งหน้าเว็บแก้สิทธิ์ &7· ใส่ที่อยู่ของบริการใน &feditor-url &7ของ &fplugins/data/perms/config.toml &7แล้ว &f/lp sync &7หรือใช้คำสั่ง &f/lp &7แทน", "&eNo web editor is set for this server &7· put the address of an editor service in &feditor-url &7of &fplugins/data/perms/config.toml&7, then &f/lp sync&7, or use the &f/lp &7commands"),
    ("editor_fail", "&cติดต่อหน้าเว็บแก้สิทธิ์ไม่ได้: &f{0} &7· ลองใหม่อีกครั้ง หรือใช้คำสั่ง &f/lp &7ไปก่อน", "&cThe web editor could not be reached: &f{0} &7· try again, or use the &f/lp &7commands meanwhile"),
    ("edits_code", "&cรหัสไม่ถูกต้อง &7(คัดลอกคำสั่งจากหน้าเว็บหลังกด บันทึก)", "&cThat is not a code &7(copy the command from the page after pressing Save)"),
    ("edits_fetch", "&7กำลังดึงการแก้ไขจากหน้าเว็บ…", "&7Fetching the changes from the web editor…"),
    ("edits_done", "&aนำการแก้ไขจากหน้าเว็บมาใช้แล้ว: กลุ่ม &f{0} &aผู้เล่น &f{1} &atrack &f{2} &aลบ &f{3}{4}", "&aChanges from the web editor applied: &f{0} &agroups, &f{1} &aplayers, &f{2} &atracks, &f{3} &adeleted{4}"),
    ("edits_fail", "&cนำการแก้ไขมาใช้ไม่ได้: &f{0}", "&cThe changes could not be applied: &f{0}"),
    ("edits_gone", "&cไม่พบการแก้ไขรหัสนี้ &7(หมดอายุหรือพิมพ์ผิด กด บันทึก ในหน้าเว็บใหม่อีกครั้ง)", "&cNo changes under that code &7(expired or mistyped; press Save on the page again)"),
    ("web_busy", "&eกำลังติดต่อหน้าเว็บอยู่ รอสักครู่แล้วลองใหม่", "&eStill talking to the web editor; try again in a moment"),
    ("help_title", "&bPerms &f{0} &7· คำสั่ง (ไวยากรณ์เดียวกับ LuckPerms)", "&bPerms &f{0} &7· commands (LuckPerms syntax)"),
    ("sync_entry", "&eรายการนี้ใช้ไม่ได้ จึงข้าม: &f{0}", "&eThis entry is not valid, skipped: &f{0}"),
    ("import_refused", " &e(ข้าม {0} รายการที่ไม่ถูกต้อง)", " &e({0} entries were not valid and were skipped)"),
    ("temp_exists", "&c{0} มี &f{1} &cแบบชั่วคราวอยู่แล้ว &7(ตัวเลือก deny)", "&c{0} already has a temporary &f{1} &7(modifier deny)"),
    ("meta_set_temp", "&aตั้ง meta &f{1} &a= &f{2} &aให้ &f{0} &aแล้ว{3}", "&aSet meta &f{1} &a= &f{2} &afor &f{0}{3}"),
    ("prefix_set_temp", "&aตั้ง {1} ลำดับ &f{2} &aเป็น &r{3}&a ให้ &f{0} &aแล้ว{4}", "&aSet {1} &f{2} &ato &r{3}&a for &f{0}{4}"),
    ("primary_auto", "&ePerms ไม่เก็บกลุ่มหลักแยก: กลุ่มหลักคือกลุ่มที่น้ำหนักมากที่สุดของผู้เล่น เปลี่ยนด้วย &f/lp group <กลุ่ม> setweight &eหรือ &fparent set", "&ePerms stores no primary group: it is the heaviest group the player is in. Change it with &f/lp group <group> setweight &eor &fparent set"),
    ("guard_regex", "&cnode แบบ regex (r=) ตั้งหรือถอนได้เฉพาะ console หรือ op ระดับ 4", "&cRegex nodes (r=) can only be set or removed by the console or a level 4 operator"),
    ("log_title", "&bบันทึกการแก้ไข &7หน้า &f{0}&7/&f{1} &7({2} รายการ)", "&bAction log &7page &f{0}&7/&f{1} &7({2} entries)"),
    ("log_line", " &7#{0} &8{1} &b{2} &7[{3}] &f{4}&7: {5}", " &7#{0} &8{1} &b{2} &7[{3}] &f{4}&7: {5}"),
    ("ago", "{0}ที่แล้ว", "{0} ago"),
    ("log_empty", "&7ยังไม่มีบันทึกที่ตรงกับที่ค้น", "&7No log entries match"),
    ("log_console", "&econsole เห็นทุกการแก้ไขในบรรทัด log ของเซิร์ฟเวอร์อยู่แล้ว", "&eThe console already sees every edit in the server log"),
    ("log_notify_on", "&aจะแจ้งคุณเมื่อมีคนแก้สิทธิ์", "&aYou will be told when someone edits permissions"),
    ("log_notify_off", "&aจะไม่แจ้งคุณเมื่อมีคนแก้สิทธิ์แล้ว", "&aYou will no longer be told when someone edits permissions"),
    ("log_notice", "&8[&bPerms log&8]&r &b{0} &7[{1}] &f{2}&7: {3}", "&8[&bPerms log&8]&r &b{0} &7[{1}] &f{2}&7: {3}"),
    ("bulk_none", "&eไม่มี node ที่ตรงเงื่อนไข ไม่มีอะไรเปลี่ยน", "&eNo node matches; nothing would change"),
    ("bulk_preview", "&eจะแก้ &f{0} &enode ใน &f{1} &eกลุ่มหรือผู้เล่น &7· ยืนยันภายใน 60 วินาทีด้วย &f/lp bulkupdate confirm {2}", "&eThis would change &f{0} &enodes on &f{1} &eholders &7· confirm within 60 seconds with &f/lp bulkupdate confirm {2}"),
    ("bulk_done", "&aแก้แล้ว &f{0} &anode ใน &f{1} &aกลุ่มหรือผู้เล่น", "&aChanged &f{0} &anodes on &f{1} &aholders"),
    ("bulk_no_pending", "&cไม่มีรายการรอยืนยันที่ตรงรหัสนี้ (หรือหมดเวลาแล้ว) สั่ง bulkupdate ใหม่อีกครั้ง", "&cNo pending bulk update has that id (or it timed out); run bulkupdate again"),
    ("save_fail", "&cบันทึกไฟล์ไม่สำเร็จ {0} ไฟล์ ตรวจสิทธิ์เขียนของโฟลเดอร์ plugins/data/perms", "&cFailed to write {0} files; check that plugins/data/perms is writable"),
];

/// Lines of `/lp help`, the same in both languages apart from the notes.
pub const HELP: &[&str] = &[
    "user <name> info | permission … | parent … | meta … | promote <track> | demote <track> | clear | clone <other> | showtracks",
    "group <group> info | permission … | parent … | meta … | setweight <n> | setdisplayname <name> | rename <new> | clone <new> | listmembers | showtracks | clear",
    "… permission info | set <node> [true|false] [context…] | unset <node> [context…] | settemp <node> <true|false> <duration> [accumulate|replace|deny] [context…] | unsettemp <node> | check <node> | checkinherits <node> | clear",
    "… parent info | set <group> | add <group> [context…] | remove <group> | addtemp <group> <duration> [accumulate|replace|deny] | removetemp <group> | settrack <track> <group> | cleartrack <track> | clear",
    "… meta info | set <key> <value> | unset <key> | settemp <key> <value> <duration> | unsettemp <key> | setprefix|addprefix <priority> <text> | removeprefix <priority> | settempprefix|addtempprefix <priority> <text> <duration> | removetempprefix <priority> | (the same with suffix) | clear",
    "creategroup <name> [weight] | deletegroup <name> | listgroups",
    "createtrack <name> | deletetrack <name> | listtracks | track <track> info | append <group> | insert <group> <pos> | remove <group> | clear | rename <new> | clone <new>",
    "info | sync | check <user> <node> | tree [scope] | search <node> | export <file> | import <file> [--replace] | verbose on|off [filter] | editor | applyedits <code>",
    "log recent [user] [page] | search <text> [page] | userhistory <user> | grouphistory <group> | trackhistory <track> | notify [on|off]",
    "bulkupdate <all|users|groups> delete \"permission == <node>\" | update permission <new> \"permission ~~ <pattern>\" | confirm <id>",
    "nodes: a.b.* wildcard · r=<regex> · a.(x|y) and a.(1-5) shorthand",
];

pub fn text(th: bool, key: &str) -> &'static str {
    TEXT.iter().find(|(k, _, _)| *k == key).map_or("??", |(_, t, e)| if th { t } else { e })
}

/// Fill `{0}` … `{9}` in one pass, so a value that itself holds `{1}` is never filled again.
pub fn fill(template: &str, args: &[&str]) -> String {
    let mut out = String::with_capacity(template.len() + args.iter().map(|a| a.len()).sum::<usize>());
    let b = template.as_bytes();
    let mut i = 0;
    let mut from = 0;
    while i + 2 < b.len() {
        if b[i] == b'{' && b[i + 1].is_ascii_digit() && b[i + 2] == b'}' {
            out.push_str(&template[from..i]);
            out.push_str(args.get((b[i + 1] - b'0') as usize).copied().unwrap_or(""));
            i += 3;
            from = i;
        } else {
            i += 1;
        }
    }
    out.push_str(&template[from..]);
    out
}

pub fn msg(th: bool, key: &str, args: &[&str]) -> String {
    fill(text(th, key), args)
}

/// Why a request to the web editor failed, in words an owner can act on. `raw` is what the
/// HTTP client or the status line said (`ErrorCode::DnsError(…)`, `HTTP 429`, `timed out`).
pub fn net_reason(th: bool, raw: &str) -> String {
    let low = raw.to_ascii_lowercase();
    let pick = |thai: &str, english: &str| (if th { thai } else { english }).to_owned();
    if low.contains("dns") {
        pick("เซิร์ฟหาที่อยู่ของหน้าเว็บไม่เจอ (DNS ใช้ไม่ได้ชั่วคราว)", "the server could not look up the page's address (DNS)")
    } else if low.contains("timed out") || low.contains("timeout") {
        pick("หมดเวลารอคำตอบ", "no answer in time")
    } else if low.contains("refused") || low.contains("unreachable") || low.contains("reset") {
        pick("เชื่อมต่อออกเน็ตไม่ได้", "the connection could not be made")
    } else if low.contains("http 429") {
        pick("ส่งบ่อยเกินไป รอสักครู่", "too many requests, wait a moment")
    } else if low.contains("http 413") {
        pick("ข้อมูลใหญ่เกินไป", "the data is too large")
    } else if let Some(code) = low.strip_prefix("http ").filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_digit())) {
        pick(&format!("หน้าเว็บตอบรหัส {code}"), &format!("the page answered {code}"))
    } else {
        // Whatever it is, short and without the debug punctuation
        let plain: String = raw.chars().filter(|c| !matches!(c, '{' | '}' | '"' | '(' | ')') && !c.is_control()).take(60).collect();
        if plain.trim().is_empty() { pick("ไม่ทราบสาเหตุ", "unknown reason") } else { plain.trim().to_owned() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_errors_read_like_words() {
        let dns = "ErrorCode::DnsError(DnsErrorPayload { rcode: Some(\"address not available\"), info_code: Some(0) })";
        assert_eq!(net_reason(false, dns), "the server could not look up the page's address (DNS)");
        assert!(net_reason(true, dns).contains("DNS") && !net_reason(true, dns).contains("ErrorCode"));
        assert_eq!(net_reason(false, "timed out"), "no answer in time");
        assert_eq!(net_reason(false, "ErrorCode::ConnectionRefused"), "the connection could not be made");
        assert_eq!(net_reason(false, "HTTP 429"), "too many requests, wait a moment");
        assert_eq!(net_reason(false, "HTTP 413"), "the data is too large");
        assert_eq!(net_reason(false, "HTTP 502"), "the page answered 502");
        assert_eq!(net_reason(true, "HTTP 502"), "หน้าเว็บตอบรหัส 502");
        assert_eq!(net_reason(false, "ErrorCode::Odd(Thing { \"x\" })"), "ErrorCode::OddThing  x");
        assert_eq!(net_reason(false, "{}"), "unknown reason");
        assert!(net_reason(false, &"x".repeat(500)).len() == 60);
    }

    #[test]
    fn fill_is_single_pass() {
        assert_eq!(fill("a {0} b {1}", &["{1}", "x"]), "a {1} b x");
        assert_eq!(fill("{0}", &["v"]), "v");
        assert_eq!(fill("no holes", &["v"]), "no holes");
        assert_eq!(fill("{0}{0}{2}", &["v"]), "vv");
        assert_eq!(fill("ไทย {0} ไทย", &["ก"]), "ไทย ก ไทย");
        assert_eq!(fill("{x} {", &[]), "{x} {");
    }

    #[test]
    fn both_languages_complete() {
        let mut seen = std::collections::HashSet::new();
        for (key, th, en) in TEXT {
            assert!(seen.insert(*key), "duplicate key {key}");
            assert!(!th.is_empty() && !en.is_empty(), "{key}");
            // the same holes in both languages
            for n in 0..10 {
                let hole = format!("{{{n}}}");
                assert_eq!(th.contains(&hole), en.contains(&hole), "{key} {hole}");
            }
            assert!(!th.contains('—') && !en.contains('—'), "{key}: no em dash");
        }
        assert_eq!(text(true, "missing"), "??");
    }
}
