import os
from pathlib import Path
import stat
import tempfile
import unittest
from unittest import mock

import secure_scratch as scratch


@unittest.skipUnless(scratch._posix_available(), "descriptor-relative POSIX scratch required")
class PosixScratchTests(unittest.TestCase):
    def setUp(self):
        self.root_context = tempfile.TemporaryDirectory()
        self.addCleanup(self.root_context.cleanup)
        self.root = Path(self.root_context.name).resolve()
        self.environment = mock.patch.dict(os.environ, {"TMPDIR": str(self.root)}, clear=True)
        self.environment.start()
        self.addCleanup(self.environment.stop)

    def test_private_directory_and_unlinked_seekable_error_file(self):
        previous_umask = os.umask(0o077)
        try:
            with scratch.ScratchDirectory() as directory:
                path = Path(directory)
                self.assertEqual(path.parent, self.root)
                self.assertEqual(str(directory), directory.name)
                self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o700)
                directory.validate_path()
                with directory.anonymous_text_file() as stream:
                    self.assertEqual(os.fstat(stream.fileno()).st_nlink, 0)
                    self.assertEqual(stat.S_IMODE(os.fstat(stream.fileno()).st_mode), 0o600)
                    stream.write("a diagnostic\n")
                    stream.seek(0)
                    self.assertEqual(stream.read(), "a diagnostic\n")
                    self.assertEqual(list(path.iterdir()), [])
                descriptor = directory._directory_fd
            self.assertFalse(path.exists())
            with self.assertRaises(OSError):
                os.fstat(descriptor)
        finally:
            os.umask(previous_umask)

    def test_root_priority_and_unusable_root_fallback(self):
        second = self.root / "second"
        second.mkdir()
        os.environ["TEMP"] = str(second)
        with scratch.ScratchDirectory() as directory:
            self.assertEqual(Path(directory).parent, self.root)
        os.environ["TMPDIR"] = str(self.root / "missing")
        with scratch.ScratchDirectory() as directory:
            self.assertEqual(Path(directory).parent, second)

    def test_readable_but_unwritable_root_is_probed_then_skipped(self):
        second = self.root / "second"
        second.mkdir()
        os.environ["TEMP"] = str(second)
        original = scratch._anonymous_at
        rejected_identity = scratch._identity(self.root.stat())

        def probe(descriptor):
            if scratch._identity(os.fstat(descriptor)) == rejected_identity:
                raise PermissionError("simulated read-only root")
            return original(descriptor)

        with mock.patch.object(scratch, "_anonymous_at", side_effect=probe):
            with scratch.ScratchDirectory() as directory:
                self.assertEqual(Path(directory).parent, second)

    def test_configured_symlink_root_keeps_priority(self):
        actual = self.root / "actual"
        actual.mkdir()
        link = self.root / "configured"
        link.symlink_to(actual, target_is_directory=True)
        os.environ["TMPDIR"] = str(link)
        with scratch.ScratchDirectory() as directory:
            self.assertEqual(Path(directory).parent, actual)

    def test_setgid_root_does_not_turn_private_inherited_mode_into_an_error(self):
        os.chmod(self.root, 0o2700)
        with scratch.ScratchDirectory() as directory:
            self.assertEqual(Path(directory).stat().st_mode & 0o777, 0o700)

    def test_exclusive_directory_collision_does_not_remove_existing_entry(self):
        collision = self.root / "collision"
        collision.mkdir()
        with mock.patch.object(scratch, "_name", side_effect=["probe", "collision", "owned"]):
            with scratch.ScratchDirectory() as directory:
                self.assertEqual(Path(directory).name, "owned")
        self.assertTrue(collision.is_dir())
        self.assertFalse((self.root / "owned").exists())

    def test_nested_cleanup_never_follows_external_symlink(self):
        external = self.root / "external"
        external.mkdir()
        retained = external / "keep"
        retained.write_text("external")
        with scratch.ScratchDirectory() as directory:
            path = Path(directory)
            nested = path / "a" / "b"
            nested.mkdir(parents=True)
            (nested / "file").write_text("owned")
            (path / "link").symlink_to(external, target_is_directory=True)
            (path / "dangling").symlink_to(self.root / "absent")
        self.assertFalse(path.exists())
        self.assertEqual(retained.read_text(), "external")

    def test_replaced_root_name_is_not_deleted_and_descriptors_close(self):
        owner = scratch.ScratchDirectory()
        directory = owner.__enter__()
        original = Path(directory)
        renamed = original.with_name(original.name + "-moved")
        descriptor = owner._directory_fd
        original.rename(renamed)
        original.mkdir()
        (original / "keep").write_text("replacement")
        with self.assertRaisesRegex(RuntimeError, "replaced"):
            owner.validate_path()
        with self.assertRaisesRegex(RuntimeError, "replaced"):
            owner.__exit__(None, None, None)
        self.assertEqual((original / "keep").read_text(), "replacement")
        self.assertTrue(renamed.is_dir())
        with self.assertRaises(OSError):
            os.fstat(descriptor)

    def test_pre_yield_open_failure_removes_identified_directory(self):
        original_open = os.open

        def opening(path, flags, *args, **kwargs):
            if kwargs.get("dir_fd") is not None and flags & os.O_DIRECTORY:
                raise PermissionError("cannot reopen owned directory")
            return original_open(path, flags, *args, **kwargs)

        owner = scratch.ScratchDirectory()
        with mock.patch.object(scratch, "_posix_available", return_value=True):
            with mock.patch.object(os, "open", side_effect=opening):
                with self.assertRaisesRegex(PermissionError, "cannot reopen"):
                    owner.__enter__()
        self.assertEqual(list(self.root.iterdir()), [])
        self.assertIsNone(owner._root_fd)
        self.assertIsNone(owner._directory_fd)

    def test_cleanup_error_does_not_mask_body_error(self):
        body = ValueError("body failure")
        cleanup = OSError("cleanup failure")
        with self.assertRaises(ValueError) as caught:
            with mock.patch.object(scratch, "_remove_contents", side_effect=cleanup):
                with scratch.ScratchDirectory():
                    raise body
        self.assertIs(caught.exception, body)
        self.assertIs(caught.exception.__cause__, cleanup)

    def test_standalone_error_file_is_anonymous_and_closes_root(self):
        with scratch.anonymous_text_file() as stream:
            self.assertEqual(os.fstat(stream.fileno()).st_nlink, 0)
            stream.write("standalone")
            stream.seek(0)
            self.assertEqual(stream.read(), "standalone")
        self.assertEqual(list(self.root.iterdir()), [])

    def test_unverifiable_created_file_is_retained_and_root_selection_stops(self):
        original_fstat = os.fstat
        descriptors = []

        def failing(descriptor):
            result = original_fstat(descriptor)
            if stat.S_ISREG(result.st_mode):
                descriptors.append(descriptor)
                raise OSError("cannot read created file identity")
            return result

        with mock.patch.object(os, "fstat", side_effect=failing):
            with self.assertRaisesRegex(OSError, "cannot read created") as caught:
                scratch.anonymous_text_file()
        self.assertTrue(any("retained" in note for note in caught.exception.__notes__))
        self.assertEqual(len(list(self.root.iterdir())), 1)
        self.assertEqual(len(descriptors), 1)
        with self.assertRaises(OSError):
            os.fstat(descriptors[0])

    def test_unverifiable_created_directory_is_retained_with_closed_descriptors(self):
        original_stat = os.stat
        owner = scratch.ScratchDirectory(prefix="identity-failure-")

        def failing(path, *args, **kwargs):
            if isinstance(path, str) and path.startswith("identity-failure-"):
                raise OSError("cannot read created directory identity")
            return original_stat(path, *args, **kwargs)

        with mock.patch.object(scratch, "_posix_available", return_value=True):
            with mock.patch.object(os, "stat", side_effect=failing):
                with self.assertRaisesRegex(OSError, "cannot read created") as caught:
                    owner.__enter__()
        self.assertTrue(any("retained" in note for note in caught.exception.__notes__))
        self.assertEqual(len(list(self.root.iterdir())), 1)
        self.assertIsNone(owner._root_fd)
        self.assertIsNone(owner._directory_fd)

    def test_replaced_file_during_cleanup_is_not_unlinked(self):
        owner = scratch.ScratchDirectory()
        path = Path(owner.__enter__())
        victim = path / "victim"
        victim.write_text("original")
        original_stat = os.stat
        reads = 0

        def replacing(name, *args, **kwargs):
            nonlocal reads
            if name == "victim" and kwargs.get("dir_fd") == owner._directory_fd:
                reads += 1
                if reads == 2:
                    victim.rename(path / "moved")
                    victim.write_text("replacement")
            return original_stat(name, *args, **kwargs)

        with mock.patch.object(os, "stat", side_effect=replacing):
            with self.assertRaisesRegex(RuntimeError, "replaced"):
                owner.__exit__(None, None, None)
        self.assertEqual(victim.read_text(), "replacement")
        self.assertEqual((path / "moved").read_text(), "original")

    def test_fdopen_failure_closes_unlinked_error_descriptor(self):
        with scratch.ScratchDirectory() as directory:
            captured = []

            def failing(descriptor, mode):
                captured.append(descriptor)
                raise OSError("fdopen failure")

            with mock.patch.object(os, "fdopen", side_effect=failing):
                with self.assertRaisesRegex(OSError, "fdopen failure"):
                    directory.anonymous_text_file()
            self.assertEqual(list(Path(directory).iterdir()), [])
            self.assertEqual(len(captured), 1)
            with self.assertRaises(OSError):
                os.fstat(captured[0])

    def test_invalid_prefix_rejected_before_creating_paths(self):
        for prefix in ("../outside", "/absolute", "nul\0", 42):
            with self.subTest(prefix=prefix), self.assertRaises(ValueError):
                scratch.ScratchDirectory(prefix)
        self.assertEqual(list(self.root.iterdir()), [])


class PortableScratchTests(unittest.TestCase):
    def test_missing_capability_retains_standard_tempfile(self):
        with mock.patch.object(scratch, "_posix_available", return_value=False):
            with scratch.ScratchDirectory() as directory:
                path = Path(directory)
                self.assertTrue(path.is_dir())
                directory.validate_path()
                with directory.anonymous_text_file() as stream:
                    stream.write("fallback")
                    stream.seek(0)
                    self.assertEqual(stream.read(), "fallback")
            self.assertFalse(path.exists())
            with scratch.anonymous_text_file() as stream:
                stream.write("standalone fallback")
                stream.seek(0)
                self.assertEqual(stream.read(), "standalone fallback")


if __name__ == "__main__":
    unittest.main()
