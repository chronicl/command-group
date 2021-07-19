use std::{
	convert::TryInto,
	io::{Error, Read, Result},
	os::unix::{
		io::{AsRawFd, RawFd},
		process::ExitStatusExt,
	},
	process::{Child, ChildStderr, ChildStdin, ChildStdout, ExitStatus},
};

use nix::{
	errno::Errno,
	libc,
	poll::{poll, PollFd, PollFlags},
	sys::{
		signal::{killpg, Signal},
		wait::WaitPidFlag,
	},
	unistd::Pid,
};

pub(super) struct ChildImp {
	pgid: Pid,
	inner: Child,
}

impl ChildImp {
	pub(super) fn new(inner: Child) -> Self {
		Self {
			pgid: Pid::from_raw(inner.id().try_into().expect("Command PID > i32::MAX")),
			inner,
		}
	}

	pub(super) fn take_stdin(&mut self) -> Option<ChildStdin> {
		self.inner.stdin.take()
	}

	pub(super) fn take_stdout(&mut self) -> Option<ChildStdout> {
		self.inner.stdout.take()
	}

	pub(super) fn take_stderr(&mut self) -> Option<ChildStderr> {
		self.inner.stderr.take()
	}

	pub fn inner(&mut self) -> &mut Child {
		&mut self.inner
	}

	pub fn into_inner(self) -> Child {
		self.inner
	}

	pub(super) fn signal_imp(&mut self, sig: Signal) -> Result<()> {
		killpg(self.pgid, sig).map_err(Error::from)
	}

	pub fn kill(&mut self) -> Result<()> {
		self.signal_imp(Signal::SIGKILL)
	}

	pub fn id(&self) -> u32 {
		self.inner.id()
	}

	fn wait_imp(&mut self, flag: WaitPidFlag) -> Result<i32> {
		let negpid = Pid::from_raw(-self.pgid.as_raw());

		// we can't use the safe wrapper directly because it doesn't return the raw status, and we
		// need it to convert to the std's ExitStatus.
		let mut status: i32 = 0;
		let res =
			unsafe { libc::waitpid(negpid.into(), &mut status as *mut libc::c_int, flag.bits()) };

		Errno::result(res).map_err(Error::from).map(|_| status)
	}

	pub fn wait(&mut self) -> Result<ExitStatus> {
		self.wait_imp(WaitPidFlag::empty())
			.map(ExitStatus::from_raw)
	}

	pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
		self.wait_imp(WaitPidFlag::WNOHANG)
			.map(|status| match status {
				0 => None,
				s => Some(ExitStatus::from_raw(s)),
			})
	}

	pub(super) fn read_both(
		mut out_r: ChildStdout,
		out_v: &mut Vec<u8>,
		mut err_r: ChildStderr,
		err_v: &mut Vec<u8>,
	) -> Result<()> {
		let out_fd = out_r.as_raw_fd();
		let err_fd = err_r.as_raw_fd();
		set_nonblocking(out_fd, true)?;
		set_nonblocking(err_fd, true)?;

		let mut fds = [
			PollFd::new(out_fd, PollFlags::POLLIN),
			PollFd::new(err_fd, PollFlags::POLLIN),
		];

		loop {
			poll(&mut fds, -1)?;

			if fds[0].revents().is_some() && read(&mut out_r, out_v)? {
				set_nonblocking(err_fd, false)?;
				return err_r.read_to_end(err_v).map(drop);
			}
			if fds[1].revents().is_some() && read(&mut err_r, err_v)? {
				set_nonblocking(out_fd, false)?;
				return out_r.read_to_end(out_v).map(drop);
			}
		}

		fn read(r: &mut impl Read, dst: &mut Vec<u8>) -> Result<bool> {
			match r.read_to_end(dst) {
				Ok(_) => Ok(true),
				Err(e) => {
					if e.raw_os_error() == Some(libc::EWOULDBLOCK)
						|| e.raw_os_error() == Some(libc::EAGAIN)
					{
						Ok(false)
					} else {
						Err(e)
					}
				}
			}
		}

		#[cfg(target_os = "linux")]
		fn set_nonblocking(fd: RawFd, nonblocking: bool) -> Result<()> {
			let v = nonblocking as libc::c_int;
			let res = unsafe { libc::ioctl(fd, libc::FIONBIO, &v) };

			Errno::result(res).map_err(Error::from).map(drop)
		}

		#[cfg(not(target_os = "linux"))]
		fn set_nonblocking(fd: RawFd, nonblocking: bool) -> Result<()> {
			let mut flags = OFlag::from_bits(fcntl(fd, FcntlArg::GETFL)?);
			flags.set(OFlag::O_NONBLOCK, nonblocking);
			fcntl(fd, FcntlArg::SETFL(flags)).map_err(Error::from)
		}
	}
}

pub trait UnixChildExt {
	fn signal(&mut self, sig: Signal) -> Result<()>;
}

impl UnixChildExt for ChildImp {
	fn signal(&mut self, sig: Signal) -> Result<()> {
		self.signal_imp(sig)
	}
}