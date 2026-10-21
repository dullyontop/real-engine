// WASI preview1 imports for the C++ side (Luau, Jolt) in a browser: no environment,
// clocks from Date/performance, stdout and stderr to the console, no files, no sleeping.
let memory = null;
export function setMemory(m) { memory = m; }
const getMemory = () => memory;
const write = (fd, s) => (fd === 2 ? console.error : console.log)(s);
const ENOSYS = 52, EBADF = 8;
const dec = new TextDecoder();
const imports = {
    environ_get: () => 0,
    environ_sizes_get: (countPtr, sizePtr) => {
      const dv = new DataView(getMemory().buffer);
      dv.setUint32(countPtr, 0, true); dv.setUint32(sizePtr, 0, true); return 0;
    },
    clock_time_get: (id, _precision, outPtr) => {
      const ns = id === 0 ? BigInt(Date.now()) * 1000000n : BigInt(Math.round(performance.now() * 1e6));
      new DataView(getMemory().buffer).setBigUint64(outPtr, ns, true); return 0;
    },
    fd_close: () => EBADF,
    fd_fdstat_get: (fd, outPtr) => {
      if (fd > 2) return EBADF;
      const dv = new DataView(getMemory().buffer);
      dv.setUint8(outPtr, 2); // character device
      dv.setUint16(outPtr + 2, 0, true); dv.setBigUint64(outPtr + 8, 0n, true); dv.setBigUint64(outPtr + 16, 0n, true);
      return 0;
    },
    fd_seek: () => ENOSYS,
    fd_prestat_get: () => EBADF, // no preopened directories: no files
    fd_prestat_dir_name: () => EBADF,
    poll_oneoff: () => ENOSYS, // sleeping is not possible on the main thread
    sched_yield: () => 0,
    fd_write: (fd, iovs, iovsLen, nwrittenPtr) => {
      const mem = getMemory(); const dv = new DataView(mem.buffer);
      let n = 0, s = '';
      for (let i = 0; i < iovsLen; i++) {
        const p = dv.getUint32(iovs + i * 8, true), l = dv.getUint32(iovs + i * 8 + 4, true);
        s += dec.decode(new Uint8Array(mem.buffer, p, l)); n += l;
      }
      write(fd, s.replace(/\n$/, '')); dv.setUint32(nwrittenPtr, n, true); return 0;
    },
    proc_exit: (code) => { throw new Error(`proc_exit(${code})`); },
};
export function environ_get(...a) { return imports.environ_get(...a); }
export function environ_sizes_get(...a) { return imports.environ_sizes_get(...a); }
export function clock_time_get(...a) { return imports.clock_time_get(...a); }
export function fd_close(...a) { return imports.fd_close(...a); }
export function fd_fdstat_get(...a) { return imports.fd_fdstat_get(...a); }
export function fd_prestat_get(...a) { return imports.fd_prestat_get(...a); }
export function fd_prestat_dir_name(...a) { return imports.fd_prestat_dir_name(...a); }
export function fd_seek(...a) { return imports.fd_seek(...a); }
export function fd_write(...a) { return imports.fd_write(...a); }
export function poll_oneoff(...a) { return imports.poll_oneoff(...a); }
export function proc_exit(...a) { return imports.proc_exit(...a); }
export function sched_yield(...a) { return imports.sched_yield(...a); }
