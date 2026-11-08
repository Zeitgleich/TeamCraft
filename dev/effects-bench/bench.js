// The page side of the effects bench (#1336; build.sh says how to run it). A 1280x720 canvas
// draws a moving test picture; its captureStream() plays in a <video>, which the bench module
// (crates/teamcraft-media/examples/effects_bench.rs) takes its pictures from, as a call takes the
// camera's.
import init, { run } from './effects_bench.js';

const log = document.getElementById('log');
const say = (line) => { log.textContent += line + '\n'; };

function testPicture() {
  const canvas = document.createElement('canvas');
  canvas.width = 1280;
  canvas.height = 720;
  const context = canvas.getContext('2d');
  let frame = 0;
  const draw = () => {
    frame += 1;
    const gradient = context.createLinearGradient(0, 0, 1280, 720);
    gradient.addColorStop(0, `hsl(${frame % 360}, 60%, 55%)`);
    gradient.addColorStop(1, `hsl(${(frame + 180) % 360}, 60%, 35%)`);
    context.fillStyle = gradient;
    context.fillRect(0, 0, 1280, 720);
    for (let i = 0; i < 40; i += 1) {
      context.fillStyle = `hsl(${(i * 37 + frame) % 360}, 70%, 60%)`;
      context.fillRect((i * 131 + frame * 3) % 1280, (i * 71) % 720, 60, 40);
    }
    // Head and shoulders, moving a little.
    const x = 640 + Math.sin(frame / 20) * 120;
    context.fillStyle = '#e0b090';
    context.beginPath();
    context.ellipse(x, 300, 110, 140, 0, 0, Math.PI * 2);
    context.fill();
    context.fillStyle = '#304060';
    context.beginPath();
    context.ellipse(x, 720, 300, 260, 0, 0, Math.PI * 2);
    context.fill();
  };
  draw();
  setInterval(draw, 33);
  return canvas.captureStream(30);
}

async function playing(stream) {
  const video = document.createElement('video');
  video.muted = true;
  video.srcObject = stream;
  await video.play();
  while (video.videoWidth === 0) {
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  return video;
}

async function main() {
  await init();
  const files = await (await fetch('./files.json')).json();
  const video = await playing(testPicture());
  const runOne = async (compositing) => {
    say(`${compositing}: running…`);
    await fetch('./start', { method: 'POST' }).catch(() => {});
    try {
      const summary = await run(video, { compositing, frames: 165, effect: 'strong', ...files });
      say(summary);
      await fetch('./result', { method: 'POST', body: JSON.stringify({ userAgent: navigator.userAgent, compositing, summary: JSON.parse(summary) }) }).catch(() => {});
    } catch (error) {
      say(`${compositing} failed: ${error}`);
      await fetch('./result', { method: 'POST', body: JSON.stringify({ userAgent: navigator.userAgent, compositing, error: String(error) }) }).catch(() => {});
    }
  };
  const runAll = async (list) => {
    for (const button of document.querySelectorAll('button')) button.disabled = true;
    for (const compositing of list) await runOne(compositing);
    for (const button of document.querySelectorAll('button')) button.disabled = false;
    say('done');
    window.benchDone = true;
  };
  document.getElementById('both').onclick = () => runAll(['webgpu', 'webgl2']);
  document.getElementById('webgpu').onclick = () => runAll(['webgpu']);
  document.getElementById('webgl2').onclick = () => runAll(['webgl2']);
  log.textContent = `${navigator.userAgent}\nWebGPU: ${'gpu' in navigator}\n`;
  const auto = new URLSearchParams(location.search).get('run');
  if (auto) {
    await runAll(auto.split(','));
  } else {
    for (const button of document.querySelectorAll('button')) button.disabled = false;
  }
}

main().catch((error) => say(`failed: ${error}`));
