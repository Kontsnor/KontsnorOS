import json
import pytest
from unittest.mock import MagicMock, patch
import test_apt


def test_hmp_success():
    mock_qmp = MagicMock()
    mock_qf = MagicMock()
    mock_qf.readline.side_effect = [
        json.dumps({"return": "VM status: running\n"}) + "\n"
    ]

    result = test_apt.hmp("info status", qmp_sock=mock_qmp, qf_file=mock_qf)

    assert result == "VM status: running\n"
    mock_qmp.sendall.assert_called_once_with(
        b'{"execute": "human-monitor-command", "arguments": {"command-line": "info status"}}\n'
    )


def test_hmp_skips_async_events():
    mock_qmp = MagicMock()
    mock_qf = MagicMock()
    mock_qf.readline.side_effect = [
        json.dumps({"event": "RTC_CHANGE", "data": {"offset": 0}}) + "\n",
        json.dumps({"return": "CPU#0: RIP=0010:0000000000000000"}) + "\n"
    ]

    result = test_apt.hmp("info registers", qmp_sock=mock_qmp, qf_file=mock_qf)

    assert result == "CPU#0: RIP=0010:0000000000000000"
    assert mock_qf.readline.call_count == 2


def test_hmp_eof():
    mock_qmp = MagicMock()
    mock_qf = MagicMock()
    mock_qf.readline.side_effect = [""]

    result = test_apt.hmp("info status", qmp_sock=mock_qmp, qf_file=mock_qf)

    assert result is None


def test_hmp_malformed_json():
    mock_qmp = MagicMock()
    mock_qf = MagicMock()
    mock_qf.readline.side_effect = ["not valid json\n"]

    with pytest.raises(json.JSONDecodeError):
        test_apt.hmp("info status", qmp_sock=mock_qmp, qf_file=mock_qf)


def test_hmp_global_variables_fallback():
    mock_qmp = MagicMock()
    mock_qf = MagicMock()
    mock_qf.readline.side_effect = [
        json.dumps({"return": "running"}) + "\n"
    ]

    with patch.object(test_apt, 'qmp', mock_qmp, create=True), \
         patch.object(test_apt, 'qf', mock_qf, create=True):
        result = test_apt.hmp("info status")
        assert result == "running"
