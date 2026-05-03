
import pytest
from pathlib import Path
import sys
from unittest.mock import MagicMock, patch, PropertyMock

# Add project root to sys.path
sys.path.insert(0, str(Path(__file__).parents[2]))

import scripts.archive_stats as archive_stats
import scripts.check_training_readiness as check_training_readiness
import scripts.chaos_runner as chaos_runner
import scripts.discover_yang_paths as discover_yang_paths

def test_archive_stats_no_files(capsys):
    """Test archive_stats.py when no parquet files are found."""
    with patch("scripts.archive_stats.Path.rglob", return_value=[]):
        with patch("scripts.archive_stats.argparse.ArgumentParser.parse_args") as mock_args:
            mock_args.return_value = MagicMock(archive_root="archive")
            exit_code = archive_stats.main()
            assert exit_code == 0
            captured = capsys.readouterr()
            assert "No parquet files found" in captured.out

def test_archive_stats_pyarrow_missing(capsys):
    """Test archive_stats.py when pyarrow is not installed."""
    with patch("scripts.archive_stats.argparse.ArgumentParser.parse_args") as mock_args:
        mock_args.return_value = MagicMock(archive_root="archive")
        with patch.dict("sys.modules", {"pyarrow": None, "pyarrow.parquet": None}):
            exit_code = archive_stats.main()
    assert exit_code == 1
    captured = capsys.readouterr()
    assert "pyarrow" in captured.out

def test_check_training_readiness_load_config():
    """Test check_training_readiness.py config loading."""
    with patch("scripts.check_training_readiness.Path.exists", return_value=True):
        with patch("scripts.check_training_readiness.Path.read_text", return_value='api_addr = "1.2.3.4:50051"'):
            api, http = check_training_readiness._load_local_endpoints()
            assert api == "1.2.3.4:50051"

def test_check_training_readiness_format_check():
    """Test check_training_readiness.py formatting helper."""
    from bonsai_sdk.training_readiness import ReadinessCheck
    check = ReadinessCheck(name="Model A", ready=True, stats={"rows": 100}, problems=[])
    # The format_check is imported from bonsai_sdk
    from bonsai_sdk.training_readiness import format_check as fc
    output = fc(check)
    assert "READY" in output
    assert "Model A" in output
    assert "rows: 100" in output

@patch("scripts.check_training_readiness._query_via_grpc")
@patch("scripts.check_training_readiness._load_local_endpoints")
@patch("scripts.check_training_readiness.argparse.ArgumentParser.parse_args")
def test_check_training_readiness_main_success(mock_args, mock_load, mock_grpc, capsys):
    """Test check_training_readiness.main() happy path."""
    mock_args.return_value = MagicMock(api="[::1]:50051", http="...", transport="grpc")
    mock_load.return_value = ("[::1]:50051", "...")
    
    from bonsai_sdk.training_readiness import ReadinessCheck
    ready = ReadinessCheck(name="M", ready=True, stats={"ok": True}, problems=[])
    mock_grpc.return_value = (ready, ready)
    
    # main() doesn't raise SystemExit(0) on success, it just returns.
    check_training_readiness.main()
    captured = capsys.readouterr()
    assert "source: gRPC" in captured.out

def test_chaos_runner_validate_plan():
    """Test chaos_runner.py plan validation."""
    valid_plan = {"faults": [{"type": "bgp", "weight": 1}]}
    chaos_runner._validate_plan(valid_plan)
    
    invalid_plan = {"no_faults": []}
    with pytest.raises(ValueError, match="missing required key"):
        chaos_runner._validate_plan(invalid_plan)
    
    missing_type = {"faults": [{"weight": 1}]}
    with pytest.raises(ValueError, match="missing 'type'"):
        chaos_runner._validate_plan(missing_type)

def test_chaos_runner_random_from_range():
    """Test chaos_runner.py random_from_range helper."""
    assert chaos_runner.random_from_range(10) == 10
    val = chaos_runner.random_from_range([1, 5])
    assert 1 <= val <= 5

def test_chaos_runner_weighted_choice():
    """Test chaos_runner.py weighted_choice helper."""
    faults = [{"type": "a", "weight": 100}, {"type": "b", "weight": 0.001}]
    choice = chaos_runner.weighted_choice(faults)
    assert choice["type"] == "a"

@patch("scripts.chaos_runner.shutil.which", return_value=None)
def test_chaos_runner_filter_supported(mock_which):
    """Test chaos_runner.py filtering of unsupported faults."""
    faults = [
        {"type": "interface_shut"},
        {"type": "netem_loss"}, # Requires clab
        {"type": "gradual_degradation"}, # Not implemented
    ]
    supported, skipped = chaos_runner.filter_supported_faults(faults)
    assert len(supported) == 1
    assert supported[0]["type"] == "interface_shut"
    assert len(skipped) == 2
    types = [s[0] for s in skipped]
    assert "netem_loss" in types
    assert "gradual_degradation" in types

def test_chaos_runner_ns_to_iso():
    """Test chaos_runner.py timestamp conversion."""
    from datetime import timezone
    ns = 1713605570000000000 # 2024-04-20T09:32:50Z
    iso = chaos_runner._ns_to_iso(ns)
    assert "2024-04-20T09:32:50" in iso
    assert chaos_runner._ns_to_iso(None) == ""

@patch("scripts.chaos_runner.time.time_ns", return_value=12345)
@patch("scripts.chaos_runner.inject_fault.dispatch_bgp_down")
def test_chaos_runner_inject_bgp(mock_dispatch, mock_time):
    """Test chaos_runner.py injection dispatch."""
    fault = {"type": "bgp_session_down", "targets": ["h1"], "peer_addresses": ["p1"]}
    targets = {"h1": {}}
    record = chaos_runner.inject(fault, targets, "topo", dry_run=False)
    assert record["fault_type"] == "bgp_session_down"
    assert record["hostname"] == "h1"
    assert record["param"] == "p1"
    assert record["injected_at_ns"] == 12345
    mock_dispatch.assert_called_once()

@patch("scripts.chaos_runner.inject_fault.dispatch_bgp_up")
def test_chaos_runner_heal_bgp(mock_dispatch):
    """Test chaos_runner.py healing dispatch."""
    record = {"fault_type": "bgp_session_down", "hostname": "h1", "param": "p1"}
    fault = {} # def not used for bgp heal
    targets = {"h1": {}}
    chaos_runner.heal(record, fault, targets, "topo", dry_run=False)
    assert record["healed_at_ns"] is not None
    mock_dispatch.assert_called_once()

def test_discover_yang_paths_preflight(capsys):
    """Test discover_yang_paths.py preflight check."""
    with patch("scripts.discover_yang_paths.shutil.which", return_value=None):
        with pytest.raises(SystemExit) as exc:
            discover_yang_paths.preflight_check()
        assert exc.value.code == 2
        captured = capsys.readouterr()
        assert "pyang is required" in captured.err

@patch("scripts.discover_yang_paths.shutil.which", return_value="/usr/bin/pyang")
def test_discover_yang_paths_preflight_ok(mock_which):
    discover_yang_paths.preflight_check()

def test_discover_yang_paths_meta():
    """Test discover_yang_paths.py metadata generation."""
    meta = discover_yang_paths.DiscoveryMeta(repo="r", revision="rev")
    assert meta.repo == "r"
    assert meta.revision == "rev"
    d = meta.to_dict()
    assert d["repo"] == "r"
    assert d["needs_lab_verification"] is True
