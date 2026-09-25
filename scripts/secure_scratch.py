"""Descriptor-owned POSIX scratch space with a lazy portable fallback."""
import os
import stat


def _posix_available():
    return (
        os.name == "posix"
        and all(hasattr(os, name) for name in ("O_DIRECTORY", "O_NOFOLLOW", "O_CLOEXEC"))
        and all(function in os.supports_dir_fd for function in
                (os.open, os.mkdir, os.stat, os.unlink, os.rmdir))
        and os.listdir in os.supports_fd
    )


def _directory_flags():
    return os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC


def _identity(info):
    return info.st_dev, info.st_ino


def _check_identity(expected, actual):
    if _identity(expected) != _identity(actual):
        raise RuntimeError("scratch path was replaced")


def _name(prefix):
    return prefix + os.urandom(12).hex()


def _anonymous_at(directory_fd):
    flags = os.O_CREAT | os.O_EXCL | os.O_RDWR | os.O_NOFOLLOW | os.O_CLOEXEC
    for _ in range(100):
        name = _name("ddb-stderr-")
        try:
            descriptor = os.open(name, flags, 0o600, dir_fd=directory_fd)
        except FileExistsError:
            continue
        info = None
        linked = True
        try:
            info = os.fstat(descriptor)
            if not stat.S_ISREG(info.st_mode) or (info.st_mode & 0o777) & ~0o600:
                raise RuntimeError("scratch error file is not private and regular")
            _check_identity(info, os.stat(name, dir_fd=directory_fd, follow_symlinks=False))
            os.unlink(name, dir_fd=directory_fd)
            linked = False
            stream = os.fdopen(descriptor, "w+t")
            descriptor = None
            return stream
        except BaseException as error:
            if linked and info is None:
                # Without descriptor identity, a later pathname stat could
                # identify a replacement. Retain it and stop root selection.
                error.add_note("Unverified scratch error file retained: " + name)
                error._scratch_created_unverified = True
            cleanup_error = None
            if linked and info is not None:
                try:
                    current = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
                    _check_identity(info, current)
                    os.unlink(name, dir_fd=directory_fd)
                except FileNotFoundError:
                    pass
                except BaseException as failure:
                    cleanup_error = failure
            if descriptor is not None:
                try:
                    os.close(descriptor)
                except BaseException as failure:
                    if cleanup_error is None:
                        cleanup_error = failure
            if cleanup_error is not None:
                raise error from cleanup_error
            raise
    raise FileExistsError("could not reserve a unique scratch error file")


def _temporary_roots():
    return ([os.environ[key] for key in ("TMPDIR", "TEMP", "TMP") if os.environ.get(key)]
            + ["/tmp", "/var/tmp", "/usr/tmp", os.getcwd()])


def _open_root():
    last_error = None
    for candidate in _temporary_roots():
        descriptor = None
        try:
            # macOS /tmp and /var are commonly symlinks. Resolve the configured
            # root, then anchor all later operations to its open descriptor.
            path = os.path.realpath(os.path.abspath(candidate))
            descriptor = os.open(path, _directory_flags())
            if not stat.S_ISDIR(os.fstat(descriptor).st_mode):
                raise NotADirectoryError(path)
            with _anonymous_at(descriptor):
                pass
            result = path, descriptor
            descriptor = None
            return result
        except OSError as error:
            if getattr(error, "_scratch_created_unverified", False):
                raise
            last_error = error
        finally:
            if descriptor is not None:
                os.close(descriptor)
    raise FileNotFoundError("no usable temporary directory") from last_error


def anonymous_text_file():
    if not _posix_available():
        import tempfile
        return tempfile.TemporaryFile(mode="w+t")
    _, descriptor = _open_root()
    try:
        return _anonymous_at(descriptor)
    finally:
        os.close(descriptor)


def _remove_contents(descriptor):
    for name in os.listdir(descriptor):
        try:
            info = os.stat(name, dir_fd=descriptor, follow_symlinks=False)
        except FileNotFoundError:
            continue
        if stat.S_ISDIR(info.st_mode):
            child = os.open(name, _directory_flags(), dir_fd=descriptor)
            try:
                _check_identity(info, os.fstat(child))
                _remove_contents(child)
                _check_identity(info, os.stat(name, dir_fd=descriptor, follow_symlinks=False))
                os.rmdir(name, dir_fd=descriptor)
            finally:
                os.close(child)
        else:
            # A symlink is removed as a link; it is never opened or traversed.
            _check_identity(info, os.stat(name, dir_fd=descriptor, follow_symlinks=False))
            os.unlink(name, dir_fd=descriptor)


class ScratchDirectory:
    def __init__(self, prefix="ddb-proxy-measure-"):
        if not isinstance(prefix, str) or any(separator in prefix for separator in
                                               ("/", "\0", *([os.altsep] if os.altsep else []))):
            raise ValueError("scratch prefix must be a single path component")
        self.prefix = prefix
        self.path = self.name = None
        self._root_fd = self._directory_fd = None
        self._relative_name = self._created_info = None
        self._fallback = None

    def __fspath__(self):
        if self.path is None:
            raise RuntimeError("scratch directory is not active")
        return self.path

    def __str__(self):
        return self.__fspath__()

    def __enter__(self):
        if self.path is not None or self._root_fd is not None:
            raise RuntimeError("scratch directory is already active")
        if not _posix_available():
            import tempfile
            self._fallback = tempfile.TemporaryDirectory(prefix=self.prefix)
            self.path = self.name = self._fallback.__enter__()
            return self
        try:
            root, self._root_fd = _open_root()
            for _ in range(100):
                candidate = _name(self.prefix)
                try:
                    os.mkdir(candidate, 0o700, dir_fd=self._root_fd)
                except FileExistsError:
                    continue
                self._relative_name = candidate
                break
            else:
                raise FileExistsError("could not reserve a unique scratch directory")
            self._created_info = os.stat(self._relative_name, dir_fd=self._root_fd,
                                         follow_symlinks=False)
            if (not stat.S_ISDIR(self._created_info.st_mode)
                    or (self._created_info.st_mode & 0o777) & ~0o700):
                raise RuntimeError("scratch directory is not private")
            self._directory_fd = os.open(self._relative_name, _directory_flags(), dir_fd=self._root_fd)
            _check_identity(self._created_info, os.fstat(self._directory_fd))
            self.path = self.name = os.path.join(root, self._relative_name)
            self.validate_path()
            return self
        except BaseException as original:
            if self._relative_name is not None and self._created_info is None:
                original.add_note("Unverified scratch directory retained: " + self._relative_name)
            try:
                self._cleanup()
            except BaseException as cleanup_error:
                raise original from cleanup_error
            raise

    def validate_path(self):
        path = self.__fspath__()
        if self._fallback is not None:
            return
        descriptor = os.open(path, _directory_flags())
        try:
            _check_identity(os.fstat(self._directory_fd), os.fstat(descriptor))
        finally:
            os.close(descriptor)

    def anonymous_text_file(self):
        self.validate_path()
        if self._fallback is not None:
            import tempfile
            return tempfile.TemporaryFile(mode="w+t")
        return _anonymous_at(self._directory_fd)

    def _cleanup(self):
        try:
            if self._fallback is not None:
                self._fallback.cleanup()
            elif self._root_fd is not None and self._created_info is not None:
                current = os.stat(self._relative_name, dir_fd=self._root_fd, follow_symlinks=False)
                _check_identity(self._created_info, current)
                if self._directory_fd is not None:
                    _check_identity(self._created_info, os.fstat(self._directory_fd))
                    _remove_contents(self._directory_fd)
                _check_identity(self._created_info,
                                os.stat(self._relative_name, dir_fd=self._root_fd, follow_symlinks=False))
                os.rmdir(self._relative_name, dir_fd=self._root_fd)
        finally:
            try:
                if self._directory_fd is not None:
                    os.close(self._directory_fd)
            finally:
                if self._root_fd is not None:
                    os.close(self._root_fd)
                self._root_fd = self._directory_fd = None
                self._relative_name = self._created_info = None
                self._fallback = None
                self.path = self.name = None

    def __exit__(self, kind, value, traceback):
        try:
            self._cleanup()
        except BaseException as cleanup_error:
            if value is not None:
                raise value.with_traceback(traceback) from cleanup_error
            raise
        return False
