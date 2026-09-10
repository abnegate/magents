const REDUCED_MOTION = matchMedia('(prefers-reduced-motion: reduce)');
const SPINNER_FRAME_MS = 50;
const ANSI = /\x1b\[([\d;?]*)([A-Za-z])/g;
const SGR_CLASS = { 2: 'ansi-dim', 31: 'ansi-red', 32: 'ansi-green', 36: 'ansi-cyan' };

function sleep(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function jitter(base, spread) {
  return base + Math.random() * spread;
}

function faceMarkup() {
  return document.getElementById('face-template').innerHTML;
}

function promptLine(command) {
  const line = document.createElement('span');
  line.className = 't-prompt';
  line.innerHTML = faceMarkup();
  const text = document.createElement('span');
  text.className = 't-command';
  text.textContent = command;
  line.appendChild(text);
  return { line, text };
}

class Screen {
  constructor(element) {
    this.element = element;
    this.line = null;
    this.style = null;
  }

  clear() {
    this.element.replaceChildren();
    this.line = null;
    this.style = null;
  }

  currentLine() {
    if (!this.line) {
      this.line = document.createElement('span');
      this.line.className = 't-line';
      this.element.appendChild(this.line);
      this.element.appendChild(document.createTextNode('\n'));
    }
    return this.line;
  }

  newline() {
    this.currentLine();
    this.line = null;
  }

  carriageReturn() {
    this.currentLine().replaceChildren();
  }

  eraseToEnd() {
    this.currentLine();
  }

  text(value) {
    if (!value) {
      return;
    }
    const line = this.currentLine();
    if (this.style) {
      const span = document.createElement('span');
      span.className = this.style;
      span.textContent = value;
      line.appendChild(span);
    } else {
      line.appendChild(document.createTextNode(value));
    }
  }

  sgr(parameters) {
    const codes = parameters.split(';').map(Number);
    if (codes.includes(0) || parameters === '') {
      this.style = null;
      return;
    }
    for (const code of codes) {
      if (SGR_CLASS[code]) {
        this.style = SGR_CLASS[code];
      }
    }
  }

  write(chunk) {
    let index = 0;
    ANSI.lastIndex = 0;
    let match;
    while ((match = ANSI.exec(chunk)) !== null) {
      this.writePlain(chunk.slice(index, match.index));
      index = ANSI.lastIndex;
      if (match[2] === 'm') {
        this.sgr(match[1]);
      } else if (match[2] === 'K') {
        this.eraseToEnd();
      }
    }
    this.writePlain(chunk.slice(index));
  }

  writePlain(value) {
    let start = 0;
    for (let index = 0; index < value.length; index += 1) {
      const character = value[index];
      if (character === '\n' || character === '\r') {
        this.text(value.slice(start, index));
        if (character === '\n') {
          this.newline();
        } else {
          this.carriageReturn();
        }
        start = index + 1;
      }
    }
    this.text(value.slice(start));
  }

  prompt(command) {
    const { line, text } = promptLine(command);
    this.element.appendChild(line);
    this.element.appendChild(document.createTextNode('\n'));
    this.line = null;
    return text;
  }
}

function spinnerFrames(output) {
  const hide = output.indexOf('\x1b[?25l');
  const show = output.indexOf('\x1b[?25h');
  if (hide < 0 || show < 0) {
    return { frames: [], rest: output };
  }
  const frames = output
    .slice(hide, show)
    .split('\r')
    .filter((frame) => frame.replace(/\x1b\[[\d;?]*[A-Za-z]/g, '').trim() !== '');
  return { frames: frames.map((frame) => `\r${frame}`), rest: `\r${output.slice(show)}` };
}

async function typeCommand(screen, command) {
  const text = screen.prompt('');
  text.classList.add('t-cursor');
  for (const character of command) {
    text.textContent += character;
    await sleep(jitter(8, 14));
  }
  await sleep(120);
  text.classList.remove('t-cursor');
}

async function playOutput(screen, output) {
  const { frames, rest } = spinnerFrames(output);
  for (const frame of frames) {
    screen.write(frame);
    await sleep(SPINNER_FRAME_MS);
  }
  screen.write(rest);
}

function renderStatic(screen, steps) {
  for (const step of steps) {
    screen.prompt(step.command);
    screen.write(step.output);
  }
  screen.prompt('');
}

async function runTerminal(element, steps) {
  const screen = new Screen(element);
  if (REDUCED_MOTION.matches) {
    renderStatic(screen, steps);
    return;
  }
  for (;;) {
    screen.clear();
    for (const step of steps) {
      await typeCommand(screen, step.command);
      await playOutput(screen, step.output);
      await sleep(450);
    }
    screen.prompt('').classList.add('t-cursor');
    await sleep(6000);
  }
}

async function initTerminal() {
  const element = document.querySelector('[data-terminal-screen]');
  try {
    const response = await fetch('demo/demo.json');
    const { steps } = await response.json();
    await runTerminal(element, steps);
  } catch (error) {
    new Screen(element).prompt('').classList.add('t-cursor');
  }
}

function initTheme() {
  const toggle = document.querySelector('[data-theme-toggle]');
  const label = toggle.querySelector('[data-theme-label]');
  const sync = () => {
    const dark = document.documentElement.dataset.theme === 'dark';
    label.textContent = dark ? 'light' : 'dark';
    toggle.setAttribute('aria-label', dark ? 'switch to light theme' : 'switch to dark theme');
  };
  toggle.addEventListener('click', () => {
    const next = document.documentElement.dataset.theme === 'dark' ? 'light' : 'dark';
    document.documentElement.dataset.theme = next;
    try {
      localStorage.setItem('theme', next);
    } catch (error) {}
    sync();
  });
  sync();
}

function initCopy() {
  for (const button of document.querySelectorAll('[data-copy]')) {
    button.addEventListener('click', async () => {
      const scope = button.closest('.quick, .panel');
      const source = scope.querySelector('[data-copy-source]');
      try {
        await navigator.clipboard.writeText(source.textContent);
      } catch (error) {
        return;
      }
      button.textContent = 'copied';
      button.dataset.copied = '';
      setTimeout(() => {
        button.textContent = 'copy';
        delete button.dataset.copied;
      }, 1600);
    });
  }
}

function initTabs() {
  const root = document.querySelector('[data-tabs]');
  const tabs = [...root.querySelectorAll('[role="tab"]')];
  const select = (tab) => {
    for (const other of tabs) {
      const active = other === tab;
      other.setAttribute('aria-selected', String(active));
      other.tabIndex = active ? 0 : -1;
      document.getElementById(other.getAttribute('aria-controls')).hidden = !active;
    }
  };
  tabs.forEach((tab, index) => {
    tab.addEventListener('click', () => select(tab));
    tab.addEventListener('keydown', (event) => {
      const offset = { ArrowRight: 1, ArrowLeft: -1, Home: -index, End: tabs.length - 1 - index }[event.key];
      if (offset === undefined) {
        return;
      }
      event.preventDefault();
      const next = tabs[(index + offset + tabs.length) % tabs.length];
      select(next);
      next.focus();
    });
  });
}

function initRail() {
  const links = [...document.querySelectorAll('.rail-nav a')];
  if (!('IntersectionObserver' in window) || links.length === 0) {
    return;
  }
  const byId = new Map(links.map((link) => [link.dataset.node, link]));
  const visible = new Map();
  const update = () => {
    let best = null;
    for (const [id, ratio] of visible) {
      if (ratio > 0 && (!best || ratio > visible.get(best))) {
        best = id;
      }
    }
    for (const [id, link] of byId) {
      if (id === best) {
        link.setAttribute('aria-current', 'true');
      } else {
        link.removeAttribute('aria-current');
      }
    }
  };
  const observer = new IntersectionObserver(
    (entries) => {
      for (const entry of entries) {
        visible.set(entry.target.id, entry.intersectionRatio);
      }
      update();
    },
    { threshold: [0, 0.2, 0.4, 0.6, 0.8, 1], rootMargin: '-15% 0px -35% 0px' },
  );
  for (const id of byId.keys()) {
    const section = document.getElementById(id);
    if (section) {
      observer.observe(section);
    }
  }
}

initTheme();
initCopy();
initTabs();
initRail();
initTerminal();
