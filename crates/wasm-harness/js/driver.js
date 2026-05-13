// Driver for `wasm-harness`.
//
// Runs a WASI snapshot_preview1 wasm binary under a JavaScript shell
// (V8's `d8` or SpiderMonkey's `js`/`sm`) with a minimal in-script
// polyfill. Enough surface to host criterion benches and libtest binaries.
//
// Invocation (both engines):
//     <shell> driver.js -- <wasm-path> [program-args...]
//                              ^         ^
//                              argv[0]   forwarded to the wasm program
//
// Conventions:
// * stdout/stderr from the wasm program are forwarded line-by-line through
//   the shell's `print` / `printErr` primitives.
// * `proc_exit(code)` terminates the shell via `quit(code)`.
// * Args of the form `--env=KEY=VALUE` are stripped out and surfaced to the
//   wasm program as environment variables (the Rust runner injects them).

'use strict';

// --- Engine adapter --------------------------------------------------------
// Both shells provide `print`, `printErr`, `quit`, and `performance.now`.
// Differences (handled here): script-arg variable name, file-read primitive,
// and SpiderMonkey not stripping a leading `--` separator.

// `arguments` resolves to d8's script-arg global *only at module scope*; we
// can't move this into a function/IIFE without shadowing it with the
// function's own arguments object. SpiderMonkey doesn't have it at all and
// exposes `scriptArgs` instead.
const SCRIPT_ARGS_RAW =
  typeof scriptArgs !== 'undefined'
    ? scriptArgs
    : typeof arguments !== 'undefined'
      ? arguments
      : [];
const SCRIPT_ARGS = Array.from(SCRIPT_ARGS_RAW);
// SpiderMonkey passes the `--` separator through; d8 strips it for us.
if (SCRIPT_ARGS.length > 0 && SCRIPT_ARGS[0] === '--') SCRIPT_ARGS.shift();

function readFileBytes(path) {
  if (typeof readbuffer === 'function') {
    return new Uint8Array(readbuffer(path)); // d8 → ArrayBuffer
  }
  if (typeof os !== 'undefined' && os.file && typeof os.file.readFile === 'function') {
    return os.file.readFile(path, 'binary'); // SpiderMonkey → Uint8Array
  }
  throw new Error('no file-read primitive (need readbuffer or os.file.readFile)');
}

// --- UTF-8 helpers ---------------------------------------------------------
// Neither d8 nor the SpiderMonkey shell ship `TextEncoder`/`TextDecoder`.

function utf8Encode(s) {
  const out = [];
  for (let i = 0; i < s.length; i++) {
    let c = s.charCodeAt(i);
    if (c < 0x80) {
      out.push(c);
    } else if (c < 0x800) {
      out.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f));
    } else if (c >= 0xd800 && c <= 0xdbff && i + 1 < s.length) {
      const lo = s.charCodeAt(++i);
      const cp = 0x10000 + (((c & 0x3ff) << 10) | (lo & 0x3ff));
      out.push(
        0xf0 | (cp >> 18),
        0x80 | ((cp >> 12) & 0x3f),
        0x80 | ((cp >> 6) & 0x3f),
        0x80 | (cp & 0x3f),
      );
    } else {
      out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
    }
  }
  return new Uint8Array(out);
}

function utf8Decode(bytes) {
  let s = '';
  let i = 0;
  while (i < bytes.length) {
    const b = bytes[i++];
    if (b < 0x80) {
      s += String.fromCharCode(b);
    } else if (b < 0xc0) {
      s += '�';
    } else if (b < 0xe0) {
      const b2 = bytes[i++] & 0x3f;
      s += String.fromCharCode(((b & 0x1f) << 6) | b2);
    } else if (b < 0xf0) {
      const b2 = bytes[i++] & 0x3f;
      const b3 = bytes[i++] & 0x3f;
      s += String.fromCharCode(((b & 0x0f) << 12) | (b2 << 6) | b3);
    } else {
      const b2 = bytes[i++] & 0x3f;
      const b3 = bytes[i++] & 0x3f;
      const b4 = bytes[i++] & 0x3f;
      const cp = ((b & 0x07) << 18) | (b2 << 12) | (b3 << 6) | b4;
      const off = cp - 0x10000;
      s += String.fromCharCode(0xd800 | (off >> 10), 0xdc00 | (off & 0x3ff));
    }
  }
  return s;
}

// --- WASI constants --------------------------------------------------------

const ERRNO_SUCCESS = 0;
const ERRNO_BADF = 8;
const ERRNO_NOENT = 44;
const ERRNO_NOSYS = 52;

const FILETYPE_REGULAR_FILE = 4;

const STDOUT_FD = 1;
const STDERR_FD = 2;
const PREOPEN_ROOT_FD = 3;
const FIRST_OPENED_FD = 100;

// --- WASI shim factory -----------------------------------------------------

function createWasi({ memory, args, env, onExit }) {
  // Memory accessors are recomputed each call: wasm linear memory can grow,
  // which invalidates any previously captured DataView/Uint8Array.
  const dv = () => new DataView(memory().buffer);
  const u8 = () => new Uint8Array(memory().buffer);

  const cstr = (s) => {
    const u = utf8Encode(s);
    const out = new Uint8Array(u.length + 1);
    out.set(u);
    return out;
  };
  const argBufs = args.map(cstr);
  const envBufs = env.map(([k, v]) => cstr(`${k}=${v}`));
  const argTotalBytes = argBufs.reduce((a, b) => a + b.length, 0);
  const envTotalBytes = envBufs.reduce((a, b) => a + b.length, 0);

  // Line-buffered stdio. We accumulate bytes until a newline, then UTF-8
  // decode the line and emit it through the shell's `print`/`printErr`,
  // which always append a newline of their own.
  const stdioBuffers = { [STDOUT_FD]: [], [STDERR_FD]: [] };

  function emitLine(fd, bytes) {
    const s = utf8Decode(bytes);
    if (fd === STDERR_FD) printErr(s);
    else print(s);
  }
  function writeStdio(fd, bytes) {
    const buf = stdioBuffers[fd];
    for (let i = 0; i < bytes.length; i++) {
      const b = bytes[i];
      if (b === 0x0a) {
        emitLine(fd, buf);
        buf.length = 0;
      } else {
        buf.push(b);
      }
    }
  }
  function flushStdio(fd) {
    if (stdioBuffers[fd].length > 0) {
      emitLine(fd, stdioBuffers[fd]);
      stdioBuffers[fd].length = 0;
    }
  }

  // Tiny "discard" filesystem: every `path_open` allocates a fresh fd whose
  // writes are silently dropped, reads return EOF, seeks are no-ops. Enough
  // for criterion's baseline writes to succeed without us persisting data.
  let nextDiscardFd = FIRST_OPENED_FD;
  const discardFds = new Set();
  function allocDiscardFd() {
    const fd = nextDiscardFd++;
    discardFds.add(fd);
    return fd;
  }

  return {
    // --- args / environ ---------------------------------------------------
    args_sizes_get(argc_ptr, argv_buf_size_ptr) {
      dv().setUint32(argc_ptr, argBufs.length, true);
      dv().setUint32(argv_buf_size_ptr, argTotalBytes, true);
      return ERRNO_SUCCESS;
    },
    args_get(argv_ptr, argv_buf_ptr) {
      let p = argv_buf_ptr;
      for (let i = 0; i < argBufs.length; i++) {
        dv().setUint32(argv_ptr + i * 4, p, true);
        u8().set(argBufs[i], p);
        p += argBufs[i].length;
      }
      return ERRNO_SUCCESS;
    },
    environ_sizes_get(envc_ptr, env_buf_size_ptr) {
      dv().setUint32(envc_ptr, envBufs.length, true);
      dv().setUint32(env_buf_size_ptr, envTotalBytes, true);
      return ERRNO_SUCCESS;
    },
    environ_get(environ_ptr, environ_buf_ptr) {
      let p = environ_buf_ptr;
      for (let i = 0; i < envBufs.length; i++) {
        dv().setUint32(environ_ptr + i * 4, p, true);
        u8().set(envBufs[i], p);
        p += envBufs[i].length;
      }
      return ERRNO_SUCCESS;
    },

    // --- clocks -----------------------------------------------------------
    clock_res_get(_clock_id, result_ptr) {
      // 1µs nominal — both shells claim sub-µs `performance.now()` precision
      // but the value reaching us through f64 → ns conversion is noisier.
      dv().setBigUint64(result_ptr, 1000n, true);
      return ERRNO_SUCCESS;
    },
    clock_time_get(_clock_id, _precision, result_ptr) {
      const ns = BigInt(Math.round(performance.now() * 1e6));
      dv().setBigUint64(result_ptr, ns, true);
      return ERRNO_SUCCESS;
    },

    // --- file descriptors -------------------------------------------------
    fd_close(fd) {
      discardFds.delete(fd);
      return ERRNO_SUCCESS;
    },
    fd_fdstat_get(_fd, stat_ptr) {
      // Report regular-file filetype so `is_terminal()` returns false and
      // criterion / libtest disable ANSI colours. Rights bits are set
      // permissively because nothing on our side checks them.
      const v = dv();
      v.setUint8(stat_ptr, FILETYPE_REGULAR_FILE);
      v.setUint8(stat_ptr + 1, 0);
      v.setUint16(stat_ptr + 2, 0, true);
      v.setBigUint64(stat_ptr + 8, 0xffffffffffffffffn, true);
      v.setBigUint64(stat_ptr + 16, 0xffffffffffffffffn, true);
      return ERRNO_SUCCESS;
    },
    fd_fdstat_set_flags(_fd, _flags) { return ERRNO_SUCCESS; },
    fd_filestat_get() { return ERRNO_NOSYS; },
    fd_filestat_set_size() { return ERRNO_NOSYS; },
    fd_filestat_set_times() { return ERRNO_NOSYS; },

    // Single preopen "/" at fd=3. Without a preopen, libstd never calls
    // path_open at all, and any attempt to write to e.g. CRITERION_HOME
    // produces a "no such file or directory" error before reaching us.
    fd_prestat_get(fd, result_ptr) {
      if (fd === PREOPEN_ROOT_FD) {
        const v = dv();
        v.setUint8(result_ptr, 0); // tag: directory
        v.setUint32(result_ptr + 4, 1, true); // pr_name_len ("/")
        return ERRNO_SUCCESS;
      }
      return ERRNO_BADF;
    },
    fd_prestat_dir_name(fd, path_ptr, path_len) {
      if (fd === PREOPEN_ROOT_FD && path_len >= 1) {
        u8()[path_ptr] = 0x2f; // '/'
        return ERRNO_SUCCESS;
      }
      return ERRNO_BADF;
    },

    fd_read(_fd, _iovs_ptr, _iovs_len, nread_ptr) {
      dv().setUint32(nread_ptr, 0, true); // EOF on all reads
      return ERRNO_SUCCESS;
    },
    fd_readdir() { return ERRNO_NOSYS; },
    fd_seek(_fd, _offset, _whence, newoffset_ptr) {
      dv().setBigUint64(newoffset_ptr, 0n, true);
      return ERRNO_BADF;
    },
    fd_tell(_fd, result_ptr) {
      dv().setBigUint64(result_ptr, 0n, true);
      return ERRNO_BADF;
    },
    fd_write(fd, iovs_ptr, iovs_len, nwritten_ptr) {
      const isStdio = fd === STDOUT_FD || fd === STDERR_FD;
      const isDiscard = discardFds.has(fd);
      if (!isStdio && !isDiscard) {
        dv().setUint32(nwritten_ptr, 0, true);
        return ERRNO_BADF;
      }
      const v = dv();
      const mem = u8();
      let written = 0;
      for (let i = 0; i < iovs_len; i++) {
        const p = v.getUint32(iovs_ptr + i * 8, true);
        const len = v.getUint32(iovs_ptr + i * 8 + 4, true);
        if (isStdio) writeStdio(fd, mem.subarray(p, p + len));
        // discard fds: bytes consumed but not stored
        written += len;
      }
      dv().setUint32(nwritten_ptr, written, true);
      return ERRNO_SUCCESS;
    },
    fd_pread() { return ERRNO_NOSYS; },
    fd_pwrite() { return ERRNO_NOSYS; },
    fd_sync() { return ERRNO_SUCCESS; },
    fd_datasync() { return ERRNO_SUCCESS; },
    fd_advise() { return ERRNO_SUCCESS; },
    fd_allocate() { return ERRNO_NOSYS; },
    fd_renumber() { return ERRNO_NOSYS; },

    // --- paths ------------------------------------------------------------
    // `path_open` returns a fresh discard fd; later writes silently succeed.
    // criterion uses this for baseline persistence; libtest doesn't open
    // user paths at all during a normal run.
    path_open(
      _dirfd, _dirflags, _path_ptr, _path_len,
      _oflags, _fs_rights_base, _fs_rights_inheriting, _fs_flags,
      result_fd_ptr,
    ) {
      dv().setUint32(result_fd_ptr, allocDiscardFd(), true);
      return ERRNO_SUCCESS;
    },
    path_create_directory() { return ERRNO_SUCCESS; },
    path_remove_directory() { return ERRNO_SUCCESS; },
    path_unlink_file() { return ERRNO_SUCCESS; },
    path_rename() { return ERRNO_SUCCESS; },
    path_filestat_get() { return ERRNO_NOENT; },
    path_filestat_set_times() { return ERRNO_NOSYS; },
    path_link() { return ERRNO_NOSYS; },
    path_readlink() { return ERRNO_NOSYS; },
    path_symlink() { return ERRNO_NOSYS; },

    // --- misc -------------------------------------------------------------
    poll_oneoff() { return ERRNO_NOSYS; },
    proc_raise() { return ERRNO_NOSYS; },
    sched_yield() { return ERRNO_SUCCESS; },
    random_get(buf_ptr, buf_len) {
      const mem = u8();
      for (let i = 0; i < buf_len; i++) {
        mem[buf_ptr + i] = (Math.random() * 256) | 0;
      }
      return ERRNO_SUCCESS;
    },
    sock_accept() { return ERRNO_NOSYS; },
    sock_recv() { return ERRNO_NOSYS; },
    sock_send() { return ERRNO_NOSYS; },
    sock_shutdown() { return ERRNO_NOSYS; },

    proc_exit(code) {
      flushStdio(STDOUT_FD);
      flushStdio(STDERR_FD);
      onExit(code);
    },
  };
}

// `wasi.thread-spawn` import.
//
// Real threading on d8: each spawn becomes a new Worker that loads this same
// driver, receives the wasm module + shared memory + thread args via
// postMessage, and calls the wasm-exported `wasi_thread_start(tid, arg)`.
// Cross-thread synchronisation is handled by the wasm engine itself via
// `memory.atomic.wait`/`memory.atomic.notify` on the shared memory; we do
// not need to implement futex syscalls.
//
// SpiderMonkey's shell Worker API is different (`evalInWorker`, with a
// distinct sharing model). When we detect we're on SM we fall back to the
// stub: the program either falls back to single-threaded or fails to spawn.
function createThreadShim({ workerSpawn, sharedModule, sharedMemory, args, env, driverPath }) {
  if (!workerSpawn) {
    return { 'thread-spawn': (_start_arg) => -1 };
  }
  let nextTid = 1;
  return {
    'thread-spawn': (startArg) => {
      const tid = nextTid++;
      try {
        workerSpawn({
          module: sharedModule,
          memory: sharedMemory,
          tid,
          startArg,
          args,
          env,
          driverPath,
        });
        return tid;
      } catch (e) {
        printErr(`wasm-harness: thread-spawn failed: ${e && e.message ? e.message : e}`);
        return -1;
      }
    },
  };
}

// d8-specific: spawn a Worker that re-runs this driver in worker mode.
// Returns nothing meaningful — the worker runs detached and exits when its
// `wasi_thread_start` returns. Throws if Worker is unavailable.
function d8SpawnWorker(initMessage) {
  if (typeof Worker !== 'function') {
    throw new Error('Worker is not available in this shell');
  }
  const w = new Worker(initMessage.driverPath, { type: 'classic' });
  w.postMessage(initMessage);
}

// Detect engine capability for real threading. d8 has the global `Worker`
// constructor with `{type:'classic'}` and structured-cloneable wasm objects;
// SpiderMonkey's shell uses a different Worker API we don't target yet.
function pickWorkerSpawn() {
  const looksLikeD8 = typeof Worker === 'function' && typeof readbuffer === 'function';
  return looksLikeD8 ? d8SpawnWorker : null;
}

// --- Wasm import-section parser --------------------------------------------
//
// `WebAssembly.Module.imports()` reports module/name/kind but not memory
// limits. For `wasm32-wasip1-threads` we have to construct the shared memory
// ourselves with the exact limits the module declared, so we walk the binary
// to find them.
//
// We deliberately fail loudly on unknown import kinds rather than skipping
// past them: silently desynchronizing the parser would surface as a confusing
// "no memory import found" error in a future wasm version.

function readEnvMemoryLimits(bytes) {
  let i = 8; // skip magic (4) + version (4)

  function readLeb32() {
    let result = 0;
    let shift = 0;
    while (true) {
      if (i >= bytes.length) throw new Error('truncated LEB128');
      const b = bytes[i++];
      result |= (b & 0x7f) << shift;
      if ((b & 0x80) === 0) break;
      shift += 7;
      if (shift > 32) throw new Error('LEB128 too long for u32');
    }
    return result >>> 0;
  }
  function skipName() {
    // NOTE: `i += readLeb32()` would be wrong — JS compound assignment reads
    // the LHS value before evaluating the RHS, so the mutation `readLeb32`
    // performs on `i` (advancing past the length byte) would be overwritten.
    const len = readLeb32();
    i += len;
  }

  const SECTION_IMPORT = 2;
  const KIND_FUNC = 0;
  const KIND_TABLE = 1;
  const KIND_MEMORY = 2;
  const KIND_GLOBAL = 3;

  while (i < bytes.length) {
    const id = bytes[i++];
    const size = readLeb32();
    if (id !== SECTION_IMPORT) {
      i += size;
      continue;
    }
    const count = readLeb32();
    for (let n = 0; n < count; n++) {
      skipName(); // module
      skipName(); // field
      const kind = bytes[i++];
      switch (kind) {
        case KIND_FUNC:
          readLeb32(); // typeidx
          break;
        case KIND_TABLE: {
          i++; // reftype byte
          const flags = readLeb32();
          readLeb32(); // min
          if (flags & 1) readLeb32(); // max
          break;
        }
        case KIND_MEMORY: {
          const flags = readLeb32();
          const initial = readLeb32();
          const maximum = (flags & 1) ? readLeb32() : 65536;
          // wasm-core spec allows at most one memory per module, so the
          // first one is the one (no need to match module/field name).
          return { initial, maximum };
        }
        case KIND_GLOBAL:
          i += 2; // valtype byte + mut byte
          break;
        default:
          throw new Error(`unsupported import kind ${kind} at offset ${i - 1}`);
      }
    }
    throw new Error('import section contained no memory import');
  }
  throw new Error('no import section found');
}

// --- Entry point -----------------------------------------------------------

function usage() {
  printErr('wasm-harness: usage: <shell> driver.js -- <wasm> [args...]');
  quit(1);
}

// Worker-mode detection: in d8 child Workers the global `postMessage` is a
// function (in main it isn't). We can't use "no script args" as the signal —
// d8 forwards the parent script's args to children verbatim.
const IS_WORKER = typeof postMessage === 'function';

if (IS_WORKER) {
  // d8's Worker API hooks `onmessage` from the global object; strict-mode
  // bare assignment would throw a ReferenceError because the binding doesn't
  // exist yet. Go through `globalThis` so we explicitly create the property.
  globalThis.onmessage = function workerOnMessage(e) {
    try {
      runWorkerThread(e.data);
    } catch (err) {
      printErr(`wasm-harness worker: ${err && err.message ? err.message : err}`);
      quit(1);
    }
  };
} else {
  mainThread(SCRIPT_ARGS);
}

function parseMainArgs(argv) {
  if (argv.length === 0) usage();
  const wasmPath = argv[0];
  let driverPath = null;
  const env = [];
  const programArgs = [];
  for (const a of argv.slice(1)) {
    if (a.startsWith('--driver-path=')) {
      driverPath = a.slice('--driver-path='.length);
    } else if (a.startsWith('--env=')) {
      const kv = a.slice('--env='.length);
      const eq = kv.indexOf('=');
      if (eq > 0) env.push([kv.slice(0, eq), kv.slice(eq + 1)]);
    } else {
      programArgs.push(a);
    }
  }
  return { wasmPath, driverPath, env, programArgs };
}

function compileWasm(wasmPath) {
  let bytes;
  try {
    bytes = readFileBytes(wasmPath);
  } catch (e) {
    printErr(`wasm-harness: failed to read ${wasmPath}: ${e && e.message ? e.message : e}`);
    quit(1);
  }
  const mod = new WebAssembly.Module(bytes);
  const wantsSharedMemory = WebAssembly.Module
    .imports(mod)
    .some((imp) => imp.module === 'env' && imp.name === 'memory' && imp.kind === 'memory');
  let limits = null;
  if (wantsSharedMemory) limits = readEnvMemoryLimits(bytes);
  return { mod, wantsSharedMemory, limits };
}

function buildImports({ wasi, memory, wantsSharedMemory, threadShim }) {
  const imports = {
    wasi_snapshot_preview1: wasi,
    wasi: threadShim,
  };
  if (wantsSharedMemory) imports.env = { memory };
  return imports;
}

function mainThread(argv) {
  const { wasmPath, driverPath, env, programArgs } = parseMainArgs(argv);
  const { mod, wantsSharedMemory, limits } = compileWasm(wasmPath);

  let memory;
  if (wantsSharedMemory) {
    memory = new WebAssembly.Memory({
      initial: limits.initial,
      maximum: limits.maximum,
      shared: true,
    });
  }

  const args = [wasmPath, ...programArgs];
  const wasi = createWasi({
    memory: () => memory,
    args,
    env,
    onExit: (code) => quit(code),
  });

  // Real threading is only wired up when (a) the module asked for shared
  // memory, (b) the shell supports Worker, and (c) the runner told us where
  // the driver file is. Otherwise threadSpawn stays a stub.
  const workerSpawn =
    wantsSharedMemory && driverPath ? pickWorkerSpawn() : null;
  const threadShim = createThreadShim({
    workerSpawn,
    sharedModule: mod,
    sharedMemory: memory,
    args,
    env,
    driverPath,
  });

  let inst;
  try {
    inst = new WebAssembly.Instance(mod, buildImports({
      wasi, memory, wantsSharedMemory, threadShim,
    }));
  } catch (e) {
    printErr(`wasm-harness: instantiation failed: ${e && e.message ? e.message : e}`);
    quit(1);
  }
  if (!wantsSharedMemory) memory = inst.exports.memory;
  if (!memory) {
    printErr('wasm-harness: wasm module did not export "memory"');
    quit(1);
  }

  if (typeof inst.exports._start === 'function') {
    try {
      inst.exports._start();
      quit(0);
    } catch (e) {
      printErr(`wasm-harness: wasm trap: ${e && e.message ? e.message : e}`);
      quit(1);
    }
  } else if (typeof inst.exports._initialize === 'function') {
    inst.exports._initialize();
    quit(0);
  } else {
    printErr('wasm-harness: wasm module has no _start or _initialize export');
    quit(1);
  }
}

// Worker-mode entry. Receives the compiled wasm module + shared memory +
// thread bookkeeping from the main thread, instantiates a thread-local
// instance, and calls the wasm-side `wasi_thread_start`. When that returns
// the worker function just returns — we deliberately do *not* call `quit()`
// in workers, because in d8 `quit(code)` terminates the entire process
// (every worker plus the main thread). A normal return lets d8 idle the
// worker after `wasi_thread_start` completes.
//
// On a wasm trap or a `proc_exit` call from inside the worker we *do* call
// `quit(code)` and intentionally bring the whole process down — that matches
// WASI proc_exit semantics (which kill every thread) and is the only
// signalling mechanism we have without a cross-thread channel.
function runWorkerThread(init) {
  const { module: mod, memory, tid, startArg, args, env, driverPath } = init;

  const wasi = createWasi({
    memory: () => memory,
    args,
    env,
    onExit: (code) => quit(code),
  });

  // Nested thread-spawn: workers can themselves spawn further threads via
  // the same mechanism.
  const workerSpawn = pickWorkerSpawn();
  const threadShim = createThreadShim({
    workerSpawn,
    sharedModule: mod,
    sharedMemory: memory,
    args,
    env,
    driverPath,
  });

  const inst = new WebAssembly.Instance(mod, buildImports({
    wasi, memory, wantsSharedMemory: true, threadShim,
  }));

  if (typeof inst.exports.wasi_thread_start !== 'function') {
    printErr('wasm-harness worker: wasm module did not export wasi_thread_start');
    quit(1);
  }
  try {
    inst.exports.wasi_thread_start(tid, startArg);
    // Normal return — let the worker idle. Do NOT call quit().
  } catch (e) {
    printErr(`wasm-harness worker tid=${tid}: trap: ${e && e.message ? e.message : e}`);
    quit(1);
  }
}
