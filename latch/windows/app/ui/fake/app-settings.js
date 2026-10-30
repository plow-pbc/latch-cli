// Fake mode for Settings, picked by the address (fake.js's own
// `update --check` stands for the default: a local build, latest 138):
//   ?update=available|fail   Check Now finds build 1342, or can't check
//   ?logout=kept|fail        Plow couldn't take this PC out, or latch refused
// A logout answers as the Mac's fake latch does: nothing to take out.

const asked = new URLSearchParams(location.search);

const failed = (code, message) => ({ error: { code, message } });

const update = asked.get("update");
const logout = asked.get("logout");

export const answers = [
  [["logout"], () => {
    if (logout === "fail") throw failed("io", "could not remove C:\\Users\\me\\AppData\\Local\\Latch\\login.json: access denied");
    return logout === "kept" ? { device: { name: "studio", removed: false } } : { device: null };
  }],
  ...(update ? [[["update", "--check"], () => {
    if (update === "fail") throw failed("plow_unreachable", "could not reach releases.plow.co");
    return { available: true, current: 1300, latest: { build: 1342, tag: "v0.1.0-1342" } };
  }]] : []),
];
