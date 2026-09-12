import os
import subprocess
from unittest.mock import MagicMock, patch

import pytest

import sync_disk


def test_sync_disk_subprocess_exception_cleanup(tmp_path):
    """Verify that when subprocess.run raises an exception, os.remove is still called in finally block."""
    temp_file = tmp_path / "cmd_file.tmp"
    temp_file.write_text("test commands\n")

    mock_tf = MagicMock()
    mock_tf.__enter__.return_value.name = str(temp_file)

    with patch("tempfile.NamedTemporaryFile", return_value=mock_tf), \
         patch("subprocess.run", side_effect=RuntimeError("Subprocess failed")), \
         patch("os.remove") as mock_remove:

        with pytest.raises(RuntimeError, match="Subprocess failed"):
            sync_disk.main()

        mock_remove.assert_called_once_with(str(temp_file))


def test_sync_disk_subprocess_success(tmp_path, capsys):
    """Verify happy path execution when subprocess.run succeeds."""
    temp_file = tmp_path / "cmd_file.tmp"

    mock_tf = MagicMock()
    mock_tf.__enter__.return_value.name = str(temp_file)

    mock_res = subprocess.CompletedProcess(
        args=["debugfs"],
        returncode=0,
        stdout="",
        stderr=""
    )

    with patch("tempfile.NamedTemporaryFile", return_value=mock_tf), \
         patch("subprocess.run", return_value=mock_res) as mock_run, \
         patch("os.remove") as mock_remove:

        sync_disk.main()

        mock_run.assert_called_once_with(
            ["debugfs", "-w", sync_disk.DISK_IMG, "-f", str(temp_file)],
            capture_output=True,
            text=True
        )
        mock_remove.assert_called_once_with(str(temp_file))

    captured = capsys.readouterr()
    assert "debugfs exited with code 0" in captured.out
    assert "Source sync completed." in captured.out


def test_sync_disk_stderr_filtering(tmp_path, capsys):
    """Verify that expected ext2 lookup noise in stderr is filtered out and real errors are printed."""
    temp_file = tmp_path / "cmd_file.tmp"

    mock_tf = MagicMock()
    mock_tf.__enter__.return_value.name = str(temp_file)

    stderr_content = (
        "rm: File not found by ext2_lookup while trying to delete /src/KontsnorOS/file1\n"
        "mkdir: File exists by ext2_lookup while creating directory /src/KontsnorOS/dir1\n"
        "debugfs: Bad magic number in super-block\n"
    )

    mock_res = subprocess.CompletedProcess(
        args=["debugfs"],
        returncode=1,
        stdout="",
        stderr=stderr_content
    )

    with patch("tempfile.NamedTemporaryFile", return_value=mock_tf), \
         patch("subprocess.run", return_value=mock_res), \
         patch("os.remove") as mock_remove:

        sync_disk.main()

        mock_remove.assert_called_once_with(str(temp_file))

    captured = capsys.readouterr()
    assert "debugfs exited with code 1" in captured.out
    assert "debugfs stderr:" in captured.out
    assert "debugfs: Bad magic number in super-block" in captured.out
    assert "File not found by ext2_lookup" not in captured.out
    assert "File exists by ext2_lookup" not in captured.out
