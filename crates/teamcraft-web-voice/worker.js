// The voice worker's script (#2140): loads the voice decoder module, whose start function takes the
// call module's messages (teamcraft-web-voice, src/lib.rs). The build names the module after its
// content (crates/teamcraft-ui/scripts/build-workers.sh) and this script too.
import init from '__WORKER_MODULE__';

init().catch((error) => {
  // A module that can't start: the page hears why and goes on concealing lost packets.
  self.postMessage({ kind: 'failed', detail: `the voice module didn't start: ${error}` });
});
