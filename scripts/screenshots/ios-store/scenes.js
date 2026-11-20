// The App Store screenshots' scenes (scripts/screenshots/ios-store.mjs): the iOS app's screens in
// light mode, drawn as HTML, with the placeholder people and texts of the Google Play screenshots
// (apps/android/app/src/androidTest/.../StoreScreenshots.kt, kept in step by hand). Icons are the
// app's own line icons (crates/teamcraft-ui/src/components/icons.rs), which ios-store.mjs passes
// to `renderScene`. No emoji serve as icons; emoji appear only as message content and reactions.

/** The scenes, in the order and with the names of the Play screenshots. */
const SCENES = {
  // "intro" is the title card: the mark, the claim and what the app does, without a device.
  iphone: ["intro", "chats", "channel", "thread", "spaces", "join-call", "settings"],
  // Google Play: the iPhone's scenes on a Pixel (camera hole, Android status bar), 1080 x 2400.
  android: ["intro", "chats", "channel", "thread", "spaces", "join-call", "settings"],
  // On iPad the split view shows the spaces next to the channel, so "spaces" has no picture of its own.
  ipad: ["intro", "chats", "channel", "thread", "join-call", "settings"],
  // The product page's creative assets: the header and the search result picture.
  header: ["header"],
  "search-result": ["search"],
};

const ME = "u1";
const PEOPLE = {
  u1: { name: "Alex Doe", hue: 215 },
  u2: { name: "Sam Doe", hue: 150 },
  u3: { name: "Kim Lee", hue: 20 },
  u4: { name: "Jo Roe", hue: 280 },
  u5: { name: "Robin Fox", hue: 190 },
  u6: { name: "Chris Day", hue: 330 },
  u7: { name: "Taylor Kay", hue: 45 },
};

// Like the i18n files, the German sample texts are the only non-English text here: German
// screenshots show German chats.
const TEXTS = {
  en: {
    intro: {
      title: "TeamCraft –",
      claim: "Team chat on your own server",
      body: "<em>Channels</em>, <em>calls</em> and <em>meetings</em>, end&#8209;to&#8209;end encrypted",
    },
    captions: {
      chats: "Team chat, <em>encrypted end&#8209;to&#8209;end</em>",
      channel: "<em>Channels</em> for every team and project",
      thread: "Keep discussions on track with <em>threads</em>",
      spaces: "Organized in <em>spaces</em>",
      "join-call": "<em>Calls</em> right from the channel",
      settings: "<em>Self-hosted</em>: your data stays with you",
    },
    tabs: ["Chats", "Channels", "Meetings", "Activity"],
    search: "Search",
    today: "Today",
    yesterday: "Yesterday",
    tuesday: "Tue",
    monday: "Mon",
    you: "You",
    channelName: "design",
    channelTopic: "Website relaunch, Project A",
    channel: [
      "New homepage mockup is ready, feedback welcome!",
      "Looks great. The hero section finally breathes. Let's ship the beta on Thursday?",
      "Agreed. I'll update the icons in the footer today.",
      "I booked a review call for 3 pm, link is in the meeting invite.",
      "Perfect, see you there 👋",
    ],
    thread: [
      "Thursday works for me. QA can start tomorrow morning.",
      "Great, I'll write the release notes.",
      "Copy for the newsletter is in the shared folder.",
      "Can we add the accessibility improvements too?",
      "Added. Thanks all, I'll tag the build tonight.",
    ],
    previews: [
      "Can you look at the draft before noon?",
      "Thanks, see you tomorrow",
      "Release notes are up",
      "I moved the meeting to 3 pm",
      "Lunch at 12?",
      "Sounds good!",
      "Sent the slides for Friday",
      "The invoice is approved",
      "Running five minutes late",
    ],
    direct: [
      "Thanks for the demo, it went really well!",
      "Morning! The newsletter draft is in the shared folder.",
      "Great, thanks. I'll read it after the stand-up.",
      "Kim added the accessibility notes as well.",
      "Perfect, I'll go through both together.",
    ],
    replies: (count) => `${count} replies`,
    threadTitle: "Thread",
    unfollow: "Unfollow",
    messageTo: (name) => `Message ${name}`,
    reply: "Reply…",
    encrypted: "End-to-end encrypted",
    callIn: (name) => `Call in #${name}`,
    inCall: "Kim Lee and Sam Doe are in the call",
    micOn: "Mic on",
    cameraOff: "Camera off",
    speaker: "Speaker",
    join: "Join",
    settings: "Settings",
    done: "Done",
    online: "Online",
    entries: [
      ["Bell", "#ff3b30", "Notifications", "Push, do not disturb, rules"],
      ["Chat", "#34c759", "Chats", "Typing indicator, search"],
      ["Sun", "#af52de", "Color theme", "Accent and contrast"],
      ["Shield", "#007aff", "Privacy and security", "Security code, app lock, app switcher"],
      ["Clock", "#ff9500", "Scheduled messages", "Messages waiting to be sent later"],
      ["Smartphone", "#8e8e93", "Devices", "Link a new device"],
    ],
    server: "Server",
    version: "App version",
    signOut: "Sign out",
  },
  de: {
    intro: {
      title: "TeamCraft –",
      claim: "Team-Chat auf eurem eigenen Server",
      body: "<em>Channels</em>, <em>Anrufe</em> und <em>Meetings</em>, Ende&#8209;zu&#8209;Ende verschlüsselt",
    },
    captions: {
      chats: "<em>Ende&#8209;zu&#8209;Ende</em> verschlüsselt",
      channel: "<em>Channels</em> für jedes Team und Projekt",
      thread: "Diskussionen bleiben in <em>Threads</em> geordnet",
      spaces: "Übersichtlich in <em>Spaces</em>",
      "join-call": "<em>Anrufe</em> direkt aus dem Channel",
      settings: "<em>Selbst gehostet</em>: Deine Daten bleiben bei dir",
    },
    tabs: ["Chats", "Channels", "Meetings", "Aktivität"],
    search: "Suchen",
    today: "Heute",
    yesterday: "Gestern",
    tuesday: "Di.",
    monday: "Mo.",
    you: "Du",
    channelName: "design",
    channelTopic: "Website-Relaunch, Projekt A",
    channel: [
      "Der neue Entwurf der Startseite ist fertig, Feedback willkommen!",
      "Sieht super aus. Der Kopfbereich wirkt endlich luftig. Beta am Donnerstag?",
      "Einverstanden. Ich passe heute noch die Icons im Footer an.",
      "Ich habe für 15 Uhr einen Review-Call angelegt, der Link steht in der Einladung.",
      "Perfekt, bis gleich 👋",
    ],
    thread: [
      "Donnerstag passt. Die QA kann morgen früh starten.",
      "Super, ich schreibe die Release Notes.",
      "Der Text für den Newsletter liegt im geteilten Ordner.",
      "Können wir die Verbesserungen bei der Barrierefreiheit noch ergänzen?",
      "Ergänzt. Danke euch, ich tagge den Build heute Abend.",
    ],
    previews: [
      "Kannst du vor Mittag auf den Entwurf schauen?",
      "Danke, bis morgen",
      "Die Release Notes sind online",
      "Ich habe das Meeting auf 15 Uhr verschoben",
      "Mittagessen um 12?",
      "Klingt gut!",
      "Die Folien für Freitag sind raus",
      "Die Rechnung ist freigegeben",
      "Ich bin fünf Minuten später da",
    ],
    direct: [
      "Danke für die Demo, die lief richtig gut!",
      "Morgen! Der Newsletter-Entwurf liegt im geteilten Ordner.",
      "Super, danke. Ich lese ihn nach dem Stand-up.",
      "Kim hat die Notizen zur Barrierefreiheit auch ergänzt.",
      "Perfekt, ich gehe beides zusammen durch.",
    ],
    replies: (count) => `${count} Antworten`,
    threadTitle: "Thread",
    unfollow: "Nicht mehr folgen",
    messageTo: (name) => `Nachricht an ${name}`,
    reply: "Antworten …",
    encrypted: "Ende-zu-Ende-verschlüsselt",
    callIn: (name) => `Anruf in #${name}`,
    inCall: "Kim Lee und Sam Doe sind im Anruf",
    micOn: "Mikro an",
    cameraOff: "Kamera aus",
    speaker: "Lautsprecher",
    join: "Beitreten",
    settings: "Einstellungen",
    done: "Fertig",
    online: "Online",
    entries: [
      ["Bell", "#ff3b30", "Benachrichtigungen", "Push, Nicht stören, Regeln"],
      ["Chat", "#34c759", "Chats", "Schreibanzeige, Suche"],
      ["Sun", "#af52de", "Farbschema", "Akzent und Kontrast"],
      ["Shield", "#007aff", "Datenschutz und Sicherheit", "Sicherheitscode, App-Sperre, App-Umschalter"],
      ["Clock", "#ff9500", "Geplante Nachrichten", "Nachrichten, die später rausgehen"],
      ["Smartphone", "#8e8e93", "Geräte", "Neues Gerät verknüpfen"],
    ],
    server: "Server",
    version: "App-Version",
    signOut: "Abmelden",
  },
};

let icons = {};
let t = TEXTS.en;

/** Draws one screenshot into #canvas; `iconPaths` maps icon names to SVG path data. */
window.renderScene = ({ device, lang, scene }, iconPaths, brand) => {
  icons = iconPaths;
  t = TEXTS[lang];
  if (!t) throw new Error(`unknown language ${lang}`);
  if (!SCENES[device]?.includes(scene)) throw new Error(`unknown scene ${device}/${scene}`);
  document.documentElement.lang = lang;
  const canvas = document.getElementById("canvas");
  // The Pixel takes the iPhone's layout (.iphone) and changes its frame and status bar (.android).
  canvas.className = device === "android" ? "canvas iphone android" : `canvas ${device}`;
  if (scene === "intro") canvas.innerHTML = introCard(brand.mark);
  else if (scene === "header") canvas.innerHTML = headerBanner(brand);
  else if (scene === "search") canvas.innerHTML = searchBanner();
  else canvas.innerHTML = `<header class="caption">${t.captions[scene]}</header>${deviceFrame(device, scene)}`;
  applyStyles();
  return document.fonts.ready.then(() => true);
};

window.SCENES = SCENES;

// ---- Scenes ----

const IPHONE = {
  chats: () => chatList(),
  channel: () => channelView(),
  thread: () => threadView({ isBackShown: true }),
  spaces: () => channelList(),
  "join-call": () => `${channelView()}<div class="scrim"></div>${joinSheet("sheet")}`,
  settings: () => `${chatList()}<div class="scrim"></div>${settingsSheet()}`,
};

const IPAD = {
  chats: () => split(chatList({ selected: "u2" }), directView()),
  channel: () => split(channelList({ selected: "design" }), channelView({ isBackShown: false })),
  thread: () => split(channelList({ selected: "design" }), threadView({ isBackShown: true })),
  "join-call": () =>
    `${split(channelList({ selected: "design" }), channelView({ isBackShown: false }))}<div class="scrim"></div>${joinSheet("sheet form")}`,
  settings: () => `${split(chatList({ selected: "u2" }), directView())}<div class="scrim"></div>${settingsSheet()}`,
};

/** The title card: mark, name and claim on the left, what it does on the right, a swipe arrow. */
/** A device with a scene of the app on its screen. */
function deviceFrame(device, scene) {
  const ui = device === "ipad" ? IPAD[scene]() : IPHONE[scene]();
  return `<div class="device"><div class="screen"><div class="ui">
      ${ui}
      ${statusBar(device)}
      ${device === "iphone" ? `<div class="island"></div>` : ""}
      ${device === "android" ? `<div class="camera-hole"></div>` : ""}
      <div class="home-indicator"></div>
    </div></div></div>`;
}

/** An iPhone showing a scene, for the banners; `className` places it. */
function phone(scene, className) {
  return `<div class="banner-phone iphone ${className}">${deviceFrame("iphone", scene)}</div>`;
}

/**
 * The product page header: the brand and the claim in the middle, which every device shows;
 * the phones at the sides may be cut off on narrow screens.
 */
function headerBanner(brand) {
  return `<section class="banner">
    ${phone("channel", "far-left")}${phone("chats", "near-left")}
    ${phone("thread", "near-right")}${phone("settings", "far-right")}
    <div class="banner-brand">
      <div class="banner-mark">${brand.mark}</div>
      <div class="banner-wordmark">${brand.wordmark}</div>
      <p class="banner-claim">${t.intro.claim}</p>
    </div>
  </section>`;
}

/** The search result picture: the claim and the app in use, in the middle. */
function searchBanner() {
  return `<section class="banner">
    <h1 class="banner-title">${t.intro.body}</h1>
    ${phone("chats", "row-left")}${phone("channel", "row-middle")}${phone("join-call", "row-right")}
  </section>`;
}

function introCard(brandMark) {
  return `<section class="intro">
    <div class="intro-mark">${brandMark}</div>
    <h1 class="intro-title">${t.intro.title}<br><em>${t.intro.claim}</em></h1>
    <p class="intro-body">${t.intro.body}</p>
    <svg class="intro-arrow" viewBox="0 0 400 40" preserveAspectRatio="none" aria-hidden="true">
      <path d="M2 20H396 M372 4l24 16-24 16"/>
    </svg>
  </section>`;
}

function split(sidebar, detail) {
  return `<div class="split"><aside class="sidebar">${sidebar}</aside><section class="detail">${detail}</section></div>`;
}

// ---- Lists ----

function chatList({ selected = null } = {}) {
  const rows = [
    { id: "u2", name: PEOPLE.u2.name, face: "u2", presence: "online", body: t.previews[0], time: "9:39", unread: 2 },
    { id: "g1", name: "Jo, Kim, Sam", group: ["u4", "u3"], body: `${t.you}: ${t.previews[1]}`, time: "8:46" },
    { id: "u3", name: PEOPLE.u3.name, face: "u3", presence: "away", body: t.previews[3], time: "6:41" },
    { id: "u5", name: PEOPLE.u5.name, face: "u5", presence: "online", body: t.previews[6], time: t.yesterday },
    { id: "u4", name: PEOPLE.u4.name, face: "u4", presence: "offline", body: `${t.you}: ${t.previews[5]}`, time: t.tuesday },
    { id: "u6", name: PEOPLE.u6.name, face: "u6", presence: "offline", body: t.previews[7], time: t.monday },
    { id: "u7", name: PEOPLE.u7.name, face: "u7", presence: "away", body: t.previews[8], time: t.monday },
  ];
  const html = rows
    .map((row, index) => {
      const isSelected = row.id === selected;
      const unread = row.unread && !isSelected;
      const face = row.group ? groupFace(row.group) : avatar(row.face, 52, row.presence);
      return `<div class="row${unread ? " unread" : ""}${isSelected ? " selected" : ""}${index === rows.length - 1 ? " last" : ""}">
        ${face}
        <div class="body">
          <div class="top"><span class="name">${row.name}</span><span class="time">${row.time}</span></div>
          <div class="bottom"><span class="preview">${row.body}</span>${unread ? `<span class="badge">${row.unread}</span>` : ""}</div>
        </div>
      </div>`;
    })
    .join("");
  return listScreen(t.tabs[0], html, 0);
}

function channelList({ selected = null } = {}) {
  const sections = [
    [
      "Project A",
      [
        { name: t.channelName, icon: "Hash", mentions: 1 },
        { name: "releases", icon: "Lock", unread: true },
        { name: "standup", icon: "Hash" },
        { name: "website-copy", icon: "Hash" },
        { name: "events", icon: "Hash" },
      ],
    ],
    [
      "Project B",
      [
        { name: "general", icon: "Hash", unread: true },
        { name: "roadmap", icon: "Hash" },
        { name: "support", icon: "Lock" },
        { name: "onboarding", icon: "Hash" },
      ],
    ],
    [
      "Team",
      [
        { name: "announcements", icon: "Hash", unread: true },
        { name: "random", icon: "Hash" },
        { name: "lunch", icon: "Hash" },
      ],
    ],
  ];
  const html = sections
    .map(
      ([space, channels]) => `
      <div class="section-title"><span>${space}</span>${icon("ChevronDown")}</div>
      ${channels
        .map((channel) => {
          const isSelected = channel.name === selected;
          const isUnread = (channel.unread || channel.mentions) && !isSelected;
          return `<div class="row channel${isUnread ? " unread" : ""}${isSelected ? " selected" : ""}">
            <span class="sigil">${icon(channel.icon)}</span>
            <div class="body"><div class="top"><span class="name">${channel.name}</span>
            ${channel.mentions && !isSelected ? `<span class="badge mention">${channel.mentions}</span>` : ""}</div></div>
          </div>`;
        })
        .join("")}`,
    )
    .join("");
  return listScreen(t.tabs[1], html, 1);
}

function listScreen(title, rows, tabIndex) {
  return `<div class="list-screen">
    <div class="toolbar">
      ${avatar(ME, 40)}
      <span class="round-button glass">${icon("Compose")}</span>
    </div>
    <div class="large-title">${title}</div>
    <div class="search">${icon("Search")}<span>${t.search}</span></div>
    <div class="rows">${rows}</div>
    ${tabBar(tabIndex)}
  </div>`;
}

function tabBar(selected) {
  const items = [
    ["Chat", t.tabs[0], 2],
    ["Hash", t.tabs[1], 0],
    ["Calendar", t.tabs[2], 0],
    ["Bell", t.tabs[3], 1],
  ];
  return `<nav class="tab-bar glass">${items
    .map(
      ([name, label, count], index) => `<span class="tab${index === selected ? " on" : ""}">
        ${icon(name)}<span>${label}</span>${count ? `<span class="tab-badge">${count}</span>` : ""}
      </span>`,
    )
    .join("")}</nav>`;
}

// ---- Conversations ----

function channelView({ isBackShown = true } = {}) {
  const feed = `
    <div class="e2ee-note">${icon("Lock")}<span>${t.encrypted}</span></div>
    <div class="day">${t.today}</div>
    ${message("u2", "9:01", [bubble(t.channel[0]), picture()])}
    ${message("u3", "9:06", [
      bubble(t.channel[1]),
      reactions([["🎉", 3], ["👍", 2, true]]),
      replies(["u2", "u4", "u1"], 4),
    ])}
    ${message("u4", "9:13", [bubble(t.channel[2]), reactions([["❤️", 2]])])}
    ${message(ME, "9:19", [bubble(t.channel[3])])}
    ${message("u2", "9:22", [bubble(t.channel[4]), reactions([["🙌", 1]])])}`;
  return conversation({
    title: `# ${t.channelName}`,
    subtitle: t.channelTopic,
    isBackShown,
    actions: `<span class="round-button">${icon("Phone")}</span>`,
    feed,
    placeholder: t.messageTo(`#${t.channelName}`),
  });
}

function threadView({ isBackShown }) {
  const feed = `
    ${message("u3", "9:06", [bubble(t.channel[1]), reactions([["🎉", 3]])])}
    <div class="thread-divider">${t.replies(5)}</div>
    ${message("u2", "9:08", [bubble(t.thread[0])])}
    ${message("u3", "9:10", [bubble(t.thread[1]), reactions([["👍", 2]])])}
    ${message("u4", "9:15", [bubble(t.thread[2])])}
    ${message("u3", "9:18", [bubble(t.thread[3]), reactions([["👀", 1]])])}
    ${message(ME, "9:20", [bubble(t.thread[4]), reactions([["✅", 3]])])}`;
  return conversation({
    title: t.threadTitle,
    subtitle: `# ${t.channelName}`,
    isBackShown,
    actions: `<span class="text-button">${t.unfollow}</span>`,
    feed,
    placeholder: t.reply,
  });
}

function directView() {
  const feed = `
    <div class="e2ee-note">${icon("Lock")}<span>${t.encrypted}</span></div>
    <div class="day">${t.yesterday}</div>
    ${message("u2", "17:48", [bubble(t.direct[0]), reactions([["🎉", 1]])])}
    <div class="day">${t.today}</div>
    ${message("u2", "9:20", [bubble(t.direct[1])])}
    ${message(ME, "9:24", [bubble(t.direct[2])])}
    ${message("u2", "9:30", [bubble(t.direct[3])])}
    ${message(ME, "9:33", [bubble(t.direct[4])])}
    ${message("u2", "9:39", [bubble(t.previews[0])])}`;
  return conversation({
    title: PEOPLE.u2.name,
    subtitle: t.online,
    isBackShown: false,
    actions: `<span class="round-button">${icon("Phone")}</span><span class="round-button">${icon("Video")}</span>`,
    feed,
    placeholder: t.messageTo(PEOPLE.u2.name),
  });
}

function conversation({ title, subtitle, isBackShown, actions, feed, placeholder }) {
  return `<div class="conversation">
    <div class="nav-bar">
      ${isBackShown ? `<span class="back">${icon("ChevronLeft")}</span>` : `<span class="spacer"></span>`}
      <div class="titles"><div class="title">${title}</div><div class="subtitle">${subtitle}</div></div>
      <span class="actions">${actions}</span>
    </div>
    <div class="feed">${feed}</div>
    <div class="composer">
      <span class="plus glass">${icon("Plus")}</span>
      <span class="field glass"><span>${placeholder}</span>${icon("Mic")}</span>
    </div>
  </div>`;
}

/** A message with its author's avatar and name (others) or right-aligned (own). */
function message(author, time, parts) {
  const isOwn = author === ME;
  const head = isOwn ? "" : `<div class="author">${PEOPLE[author].name}<span class="at">${time}</span></div>`;
  return `<div class="message${isOwn ? " own" : ""}">
    ${isOwn ? "" : avatar(author, 32)}
    <div class="stack">${head}${parts.join("")}</div>
  </div>`;
}

function bubble(text) {
  return `<div class="bubble">${emojify(text)}</div>`;
}

function picture() {
  return `<div class="picture">
    <div class="site-nav"><i></i><i></i><i></i><i></i></div>
    <div class="hero"></div>
    <div class="cards"><i></i><i></i><i></i></div>
    <span class="file">Homepage-v3.jpg</span>
  </div>`;
}

function reactions(list) {
  return `<div class="reactions">${list
    .map(([emoji, count, isMine]) => `<span class="reaction${isMine ? " mine" : ""}"><span class="emoji">${emoji}</span>${count}</span>`)
    .join("")}</div>`;
}

function replies(faces, count) {
  return `<div class="replies"><span class="faces">${faces.map((id) => avatar(id, 20)).join("")}</span><span>${t.replies(count)}</span></div>`;
}

// ---- Sheets ----

function joinSheet(className) {
  return `<div class="${className}">
    ${className === "sheet" ? `<div class="grabber"></div>` : ""}
    <div class="sheet-header">
      <div><div class="sheet-title">${t.callIn(t.channelName)}</div><div class="sheet-subtitle">${t.channelTopic}</div></div>
      <span class="close">${icon("Close")}</span>
    </div>
    <div class="in-call"><span class="faces">${avatar("u3", 30)}${avatar("u2", 30)}</span><span>${t.inCall}</span></div>
    <div class="preview-tile">
      ${avatar(ME, 112)}
      <span class="label">${PEOPLE[ME].name}</span>
      <span class="lock">${icon("Lock")}${t.encrypted}</span>
    </div>
    <div class="toggles">
      <span class="toggle"><span class="knob on">${icon("Mic")}</span>${t.micOn}</span>
      <span class="toggle"><span class="knob">${icon("VideoOff")}</span>${t.cameraOff}</span>
      <span class="toggle"><span class="knob on">${icon("Volume")}</span>${t.speaker}</span>
    </div>
    <div class="primary">${icon("Phone")}${t.join}</div>
  </div>`;
}

function settingsSheet() {
  const entries = t.entries
    .map(
      ([name, color, label, summary]) => `<div class="cell">
        <span class="tile" data-color="${color}">${icon(name)}</span>
        <div class="texts"><div class="label">${label}</div><div class="summary">${summary}</div></div>
        ${icon("ChevronRight", "chev")}
      </div>`,
    )
    .join("");
  return `<div class="sheet-card">
    <div class="inline-bar">${t.settings}<span class="done">${t.done}</span></div>
    <div class="form">
      <div class="group">
        <div class="cell profile">${avatar(ME, 60)}
          <div class="texts"><div class="label">${PEOPLE[ME].name}</div>
          <div class="summary"><span class="presence"></span>${t.online}</div></div>
        </div>
      </div>
      <div class="group">${entries}</div>
      <div class="group">
        <div class="cell plain"><div class="texts"><div class="label">${t.server}</div></div><span class="value">chat.example.com</span></div>
        <div class="cell plain"><div class="texts"><div class="label">${t.version}</div></div><span class="value">1.0.39</span></div>
      </div>
      <div class="group"><div class="cell plain"><span class="danger">${t.signOut}</span></div></div>
    </div>
  </div>`;
}

// ---- Small parts ----

function statusBar(device) {
  if (device === "android") return androidStatusBar();
  const signal = `<svg width="20" height="13" viewBox="0 0 20 13"><rect x="0" y="9" width="3.5" height="4" rx="1"/><rect x="5.5" y="6" width="3.5" height="7" rx="1"/><rect x="11" y="3" width="3.5" height="10" rx="1"/><rect x="16.5" y="0" width="3.5" height="13" rx="1"/></svg>`;
  const wifi = `<svg width="18" height="13" viewBox="0 0 18 13"><path d="M9 13l2.6-3.2a4 4 0 0 0-5.2 0z M9 6.4a6.8 6.8 0 0 1 4.6 1.8l1.6-2a9.6 9.6 0 0 0-12.4 0l1.6 2A6.8 6.8 0 0 1 9 6.4z M9 0.6A12.6 12.6 0 0 0 0.4 4l1.6 2a10 10 0 0 1 14 0l1.6-2A12.6 12.6 0 0 0 9 0.6z"/></svg>`;
  const battery = `<svg width="29" height="13" viewBox="0 0 29 13"><rect x="0.5" y="0.5" width="24" height="12" rx="3.5" fill="none" stroke="currentColor" opacity="0.4"/><rect x="2.5" y="2.5" width="20" height="8" rx="2"/><path d="M26.5 4.5v4a2 2 0 0 0 0-4z" opacity="0.4"/></svg>`;
  const right = device === "iphone" ? `${signal}${wifi}${battery}` : `${wifi}${battery}`;
  return `<div class="status-bar"><span>9:41</span><span class="right">${right}</span></div>`;
}

/**
 * The status bar of a current Pixel (Android 16): the time in bold on the left; mobile signal
 * (four rounded bars), Wi-Fi (arcs) and the battery as a pill with the level inside on the right.
 */
function androidStatusBar() {
  const signal = `<svg width="20" height="14" viewBox="0 0 20 14"><rect x="0" y="9" width="3.6" height="5" rx="1.8"/><rect x="5.4" y="6" width="3.6" height="8" rx="1.8"/><rect x="10.8" y="3" width="3.6" height="11" rx="1.8"/><rect x="16.2" y="0" width="3.6" height="14" rx="1.8"/></svg>`;
  const wifi = `<svg width="20" height="15" viewBox="0 0 20 15"><path d="M10 15l3.1-3.8a5 5 0 0 0-6.2 0z M10 7.6a7.9 7.9 0 0 1 5 1.8l1.9-2.3a10.9 10.9 0 0 0-13.8 0l1.9 2.3a7.9 7.9 0 0 1 5-1.8z M10 0.8a14.6 14.6 0 0 0-9.6 3.6l1.9 2.3a11.6 11.6 0 0 1 15.4 0l1.9-2.3A14.6 14.6 0 0 0 10 0.8z"/></svg>`;
  const battery = `<span class="pixel-battery">100</span>`;
  return `<div class="status-bar"><span class="time">9:41</span><span class="right">${signal}${wifi}${battery}</span></div>`;
}

function avatar(id, size, presence = null) {
  const person = PEOPLE[id];
  const initials = person.name
    .split(" ")
    .map((part) => part[0])
    .join("")
    .slice(0, 2);
  const dot = presence ? `<span class="dot ${presence}"></span>` : "";
  return `<span class="avatar" data-size="${size}" data-hue="${person.hue}">${initials}${dot}</span>`;
}

function groupFace(ids) {
  return `<span class="group-face">${avatar(ids[0], 36)}${avatar(ids[1], 36)}</span>`;
}

function icon(name, className = "") {
  const path = icons[name];
  if (!path) throw new Error(`no icon ${name} in icons.rs`);
  return `<svg class="icon ${className}" viewBox="0 0 24 24" aria-hidden="true"><path d="${path}"/></svg>`;
}

/** Puts emoji in message text into the colour emoji font. */
function emojify(text) {
  return text.replace(/(\p{Extended_Pictographic}️?)/gu, `<span class="emoji">$1</span>`);
}

/** Sizes and colours from the data attributes (the markup stays free of style attributes). */
function applyStyles() {
  for (const element of document.querySelectorAll(".avatar[data-size]")) {
    const size = Number(element.dataset.size);
    element.style.width = `${size}px`;
    element.style.height = `${size}px`;
    element.style.fontSize = `${Math.round(size * 0.38)}px`;
    element.style.background = `linear-gradient(160deg, hsl(${element.dataset.hue} 62% 56%), hsl(${element.dataset.hue} 58% 42%))`;
    element.removeAttribute("data-size");
  }
  for (const element of document.querySelectorAll(".tile[data-color]")) {
    element.style.background = element.dataset.color;
    element.removeAttribute("data-color");
  }
}
