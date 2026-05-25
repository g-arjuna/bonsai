"""ML Job Engine for Bonsai — APScheduler-backed scheduler with SQLite persistence.

EV1-5 T1/T4/T5/T6/T8: BonsaiJobEngine using APScheduler 4.x AsyncScheduler.

Architecture:
  - APScheduler AsyncScheduler with SQLiteJobStore at runtime/ml_jobs.db
  - On job start:  POST /api/ml/jobs  (create MlJobRun record, emit JobStarted event)
  - On job finish: PATCH /api/ml/jobs/{id} (update status/metrics)
  - Dependency chain: on_job_success_trigger() wires parent→child job chains
  - Retry: max_retries=3 with exponential back-off (5min, 15min, 45min)
  - Prometheus metrics exposed on :9201/metrics
  - Resource governor: polls GET /api/governance/pressure every 30s, pauses heavy jobs

Default schedules (created at first startup if not already in DB):
  anomaly_export_daily    cron  hour=2  minute=0
  remediation_export_weekly cron day_of_week=0 hour=2 minute=0
  gnn_inference           interval minutes=5
  syslog_embedding        interval seconds=60
  graph_snapshot          interval hours=4
  detection_clustering    cron  day_of_week=0 hour=3 minute=0
  config_embedding        interval hours=6

Run as background thread from collector_engine.py:
    engine = BonsaiJobEngine(api_url="http://localhost:3000")
    engine.start_in_background()
"""
from __future__ import annotations

import asyncio
import logging
import threading
import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Callable, Coroutine, Optional

log = logging.getLogger(__name__)

DEFAULT_API_URL = "http://localhost:3000"
DEFAULT_JOB_DB = "runtime/ml_jobs.db"
SCHEDULE_POLL_INTERVAL = 60
GOVERNANCE_POLL_INTERVAL = 30
METRICS_PORT = 9201

RETRY_DELAYS = [300, 900, 2700]
MAX_RETRIES = 3

DEFAULT_SCHEDULES = [
    {"job_id": "anomaly_export_daily",      "trigger": "cron",     "hour": 2,  "minute": 0},
    {"job_id": "remediation_export_weekly", "trigger": "cron",     "day_of_week": 0, "hour": 2, "minute": 0},
    {"job_id": "gnn_inference",             "trigger": "interval", "minutes": 5},
    {"job_id": "syslog_embedding",          "trigger": "interval", "seconds": 60},
    {"job_id": "graph_snapshot",            "trigger": "interval", "hours": 4},
    {"job_id": "detection_clustering",      "trigger": "cron",     "day_of_week": 0, "hour": 3, "minute": 0},
    {"job_id": "config_embedding",          "trigger": "interval", "hours": 6},
]

HEAVY_JOBS = {"anomaly_export_daily", "remediation_export_weekly", "stgnn_training", "detection_clustering"}


class JobState(str, Enum):
    idle = "idle"
    running = "running"
    succeeded = "succeeded"
    failed = "failed"
    cancelled = "cancelled"
    dead_letter = "dead_letter"


@dataclass
class DeadLetterJob:
    """A job that has exhausted all retries."""
    job_id: str
    run_record_id: str
    error_message: str
    failed_at: float
    retry_count: int

    def to_dict(self) -> dict:
        return {
            "job_id": self.job_id,
            "run_record_id": self.run_record_id,
            "error_message": self.error_message,
            "failed_at": self.failed_at,
            "retry_count": self.retry_count,
        }


@dataclass
class JobStatus:
    job_id: str
    state: JobState = JobState.idle
    last_run_at: Optional[float] = None
    next_run_at: Optional[float] = None
    last_outcome: str = ""
    error_message: str = ""
    run_count: int = 0
    run_record_id: str = ""

    def to_dict(self) -> dict:
        return {
            "job_id": self.job_id,
            "state": self.state.value,
            "last_run_at": self.last_run_at,
            "next_run_at": self.next_run_at,
            "last_outcome": self.last_outcome,
            "error_message": self.error_message,
            "run_count": self.run_count,
        }


class BonsaiJobEngine:
    """APScheduler-backed ML job engine with catalog, dependency chains, retries.

    Args:
        api_url: Bonsai core API URL. Used to create/update MlJobRun records
            and query governance pressure.
        job_db_path: SQLite path for APScheduler job store.
        enable_metrics: If True, start Prometheus HTTP server on :9201.
    """

    def __init__(
        self,
        api_url: str = DEFAULT_API_URL,
        job_db_path: str = DEFAULT_JOB_DB,
        enable_metrics: bool = True,
    ) -> None:
        self.api_url = api_url.rstrip("/")
        self.job_db_path = job_db_path
        self.enable_metrics = enable_metrics

        self._scheduler: Any = None
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._thread: Optional[threading.Thread] = None
        self._job_registry: dict[str, Callable] = {}
        self._job_statuses: dict[str, JobStatus] = {}
        self._dependency_chains: list[dict] = []
        self._retry_counts: dict[str, int] = {}
        self._dead_letter: list[DeadLetterJob] = []
        self._shedding_heavy: bool = False
        self._metrics: Optional[Any] = None
        self._running = False

    # ── Public API ─────────────────────────────────────────────────────────────

    def register_job(
        self,
        job_id: str,
        fn: Callable[..., Coroutine],
        trigger_type: str = "interval",
        enabled: bool = True,
        **trigger_kwargs: Any,
    ) -> None:
        """Register a job function with a trigger. Idempotent."""
        self._job_registry[job_id] = fn
        if job_id not in self._job_statuses:
            self._job_statuses[job_id] = JobStatus(job_id=job_id)
        log.debug(
            "JobEngine: registered %s (trigger=%s, enabled=%s)", job_id, trigger_type, enabled
        )

    def trigger_job(self, job_id: str) -> bool:
        """One-off trigger from UI or dependency chain."""
        if not self._scheduler or job_id not in self._job_registry:
            return False
        if self._loop:
            asyncio.run_coroutine_threadsafe(
                self._run_job(job_id), self._loop
            )
            return True
        return False

    def cancel_job(self, job_id: str) -> None:
        """Cancel a scheduled job (does not kill an in-progress run)."""
        if self._scheduler and self._loop:
            asyncio.run_coroutine_threadsafe(
                self._scheduler.remove_schedule(job_id), self._loop
            )
        if job_id in self._job_statuses:
            self._job_statuses[job_id].state = JobState.cancelled

    def get_job_status(self, job_id: str) -> Optional[JobStatus]:
        return self._job_statuses.get(job_id)

    def list_jobs(self) -> list[JobStatus]:
        return list(self._job_statuses.values())

    def list_dead_letter(self) -> list[DeadLetterJob]:
        """Return all jobs currently in the dead-letter queue."""
        return list(self._dead_letter)

    def retry_dead_letter(self, job_id: str) -> bool:
        """Operator-initiated retry of a dead-letter job. Clears it from the queue."""
        self._dead_letter = [d for d in self._dead_letter if d.job_id != job_id]
        self._retry_counts[job_id] = 0
        if job_id in self._job_statuses:
            self._job_statuses[job_id].state = JobState.idle
        return self.trigger_job(job_id)

    def on_job_success_trigger(
        self,
        parent_job_id: str,
        child_job_id: str,
        condition_fn: Optional[Callable[[Any], bool]] = None,
    ) -> None:
        """After parent succeeds, trigger child if condition_fn(result) is True."""
        self._dependency_chains.append({
            "parent": parent_job_id,
            "child": child_job_id,
            "condition_fn": condition_fn or (lambda _: True),
        })

    def start_in_background(self) -> None:
        """Start the scheduler in a daemon background thread."""
        self._thread = threading.Thread(
            target=self._run_event_loop, daemon=True, name="bonsai-job-engine"
        )
        self._thread.start()
        log.info("JobEngine: background thread started")

    def stop(self) -> None:
        self._running = False
        if self._scheduler and self._loop:
            asyncio.run_coroutine_threadsafe(self._scheduler.stop(), self._loop)

    # ── Internal async core ───────────────────────────────────────────────────

    def _run_event_loop(self) -> None:
        self._loop = asyncio.new_event_loop()
        asyncio.set_event_loop(self._loop)
        try:
            self._loop.run_until_complete(self._async_main())
        except Exception as exc:
            log.error("JobEngine event loop crashed: %s", exc)
        finally:
            self._loop.close()

    async def _async_main(self) -> None:
        await self._init_scheduler()
        await self._load_schedules_from_api()
        await self._ensure_default_schedules()

        if self.enable_metrics:
            self._start_metrics_server()

        self._running = True
        governance_task = asyncio.create_task(self._governance_loop())
        schedule_sync_task = asyncio.create_task(self._schedule_sync_loop())

        try:
            await self._scheduler.start_in_background()
            log.info("JobEngine: APScheduler started")
            while self._running:
                await asyncio.sleep(5)
        finally:
            governance_task.cancel()
            schedule_sync_task.cancel()
            await self._scheduler.stop()

    async def _init_scheduler(self) -> None:
        try:
            from apscheduler import AsyncScheduler
            from apscheduler.datastores.sqlalchemy import SQLAlchemyDataStore
            from apscheduler.eventbrokers.local import LocalEventBroker
            import sqlalchemy

            db_url = f"sqlite+aiosqlite:///{self.job_db_path}"
            import pathlib
            pathlib.Path(self.job_db_path).parent.mkdir(parents=True, exist_ok=True)

            data_store = SQLAlchemyDataStore(sqlalchemy.create_async_engine(db_url))
            event_broker = LocalEventBroker()
            self._scheduler = AsyncScheduler(
                data_store=data_store,
                event_broker=event_broker,
            )
            log.info("JobEngine: APScheduler initialised (db=%s)", self.job_db_path)
        except ImportError:
            log.warning(
                "apscheduler/sqlalchemy not installed — using in-memory fallback scheduler"
            )
            self._scheduler = _InMemoryFallbackScheduler(self)

    async def _load_schedules_from_api(self) -> None:
        """Read MlJobSchedule records from /api/ml/schedules and register them."""
        try:
            import aiohttp
            async with aiohttp.ClientSession() as session:
                async with session.get(
                    f"{self.api_url}/api/ml/schedules", timeout=aiohttp.ClientTimeout(total=10)
                ) as resp:
                    if resp.ok:
                        schedules = await resp.json()
                        for sched in schedules:
                            if sched.get("enabled") and sched.get("job_id") in self._job_registry:
                                await self._register_schedule(sched)
        except Exception as exc:
            log.debug("Could not load schedules from API: %s", exc)

    async def _ensure_default_schedules(self) -> None:
        """Create default schedules via API if they don't exist yet."""
        try:
            import aiohttp
            async with aiohttp.ClientSession() as session:
                for sched in DEFAULT_SCHEDULES:
                    try:
                        cron_expr = _schedule_to_cron(sched)
                        async with session.post(
                            f"{self.api_url}/api/ml/schedules",
                            json={
                                "job_id": sched["job_id"],
                                "cron_expr": cron_expr,
                                "enabled": True,
                            },
                            timeout=aiohttp.ClientTimeout(total=5),
                        ) as resp:
                            pass
                    except Exception:
                        pass
        except Exception as exc:
            log.debug("Could not ensure default schedules: %s", exc)

    async def _register_schedule(self, sched: dict) -> None:
        job_id = sched["job_id"]
        fn = self._job_registry.get(job_id)
        if not fn:
            return

        try:
            from apscheduler.triggers.cron import CronTrigger
            from apscheduler.triggers.interval import IntervalTrigger

            cron_expr = sched.get("cron_expr", "")
            if cron_expr:
                trigger = CronTrigger.from_crontab(cron_expr)
            else:
                trigger = IntervalTrigger(minutes=5)

            await self._scheduler.add_schedule(
                lambda jid=job_id: asyncio.ensure_future(self._run_job(jid)),
                trigger,
                id=job_id,
                conflict_policy="replace",
            )
        except Exception as exc:
            log.debug("Could not register schedule for %s: %s", job_id, exc)

    async def _run_job(self, job_id: str) -> None:
        fn = self._job_registry.get(job_id)
        if not fn:
            log.warning("JobEngine: no function registered for job_id=%s", job_id)
            return

        if self._shedding_heavy and job_id in HEAVY_JOBS:
            log.info("JobEngine: skipping %s (memory pressure shedding)", job_id)
            return

        status = self._job_statuses.setdefault(job_id, JobStatus(job_id=job_id))
        status.state = JobState.running
        status.last_run_at = time.time()
        run_record_id = await self._create_job_run_record(job_id)
        status.run_record_id = run_record_id
        status.run_count += 1

        self._emit_ml_event("job_started", {"job_id": job_id, "run_record_id": run_record_id})

        result = None
        error = ""
        started = time.time()

        try:
            from .job_progress import JobProgressReporter
            reporter = JobProgressReporter(job_id=job_id, api_url=self.api_url)
            result = await fn(reporter=reporter)
            status.state = JobState.succeeded
            status.last_outcome = "succeeded"
            status.error_message = ""
            self._retry_counts[job_id] = 0
            self._update_metrics_success(job_id, time.time() - started)
            await self._fire_dependency_chain(job_id, result)
        except Exception as exc:
            error = str(exc)
            log.error("JobEngine: job %s failed: %s", job_id, error)
            status.state = JobState.failed
            status.last_outcome = "failed"
            status.error_message = error
            self._update_metrics_failure(job_id)
            await self._schedule_retry(job_id)

        duration_ms = int((time.time() - started) * 1000)
        await self._patch_job_run_record(
            run_record_id=run_record_id,
            job_id=job_id,
            status=status.last_outcome,
            error_message=error,
            duration_ms=duration_ms,
            result=result,
        )
        self._emit_ml_event(
            "job_completed" if not error else "job_failed",
            {"job_id": job_id, "run_record_id": run_record_id, "error": error},
        )

    async def _schedule_retry(self, job_id: str) -> None:
        retries = self._retry_counts.get(job_id, 0)
        if retries >= MAX_RETRIES:
            log.error(
                "JobEngine: %s exhausted %d retries — moving to dead-letter", job_id, MAX_RETRIES
            )
            status = self._job_statuses.get(job_id)
            dlj = DeadLetterJob(
                job_id=job_id,
                run_record_id=status.run_record_id if status else "",
                error_message=status.error_message if status else "",
                failed_at=time.time(),
                retry_count=retries,
            )
            self._dead_letter = [d for d in self._dead_letter if d.job_id != job_id]
            self._dead_letter.append(dlj)
            if status:
                status.state = JobState.dead_letter
            self._emit_ml_event("job_dead_letter", {"job_id": job_id, "retries": retries,
                                                      "error": dlj.error_message})
            return

        delay = RETRY_DELAYS[min(retries, len(RETRY_DELAYS) - 1)]
        self._retry_counts[job_id] = retries + 1
        log.info("JobEngine: scheduling retry %d for %s in %ds", retries + 1, job_id, delay)

        async def _retry_after(d: int, jid: str) -> None:
            await asyncio.sleep(d)
            await self._run_job(jid)

        asyncio.ensure_future(_retry_after(delay, job_id))

    async def _fire_dependency_chain(self, parent_job_id: str, result: Any) -> None:
        for dep in self._dependency_chains:
            if dep["parent"] != parent_job_id:
                continue
            try:
                if dep["condition_fn"](result):
                    log.info(
                        "JobEngine: dependency chain: %s → triggering %s",
                        parent_job_id, dep["child"],
                    )
                    asyncio.ensure_future(self._run_job(dep["child"]))
            except Exception as exc:
                log.debug("Dependency condition check failed: %s", exc)

    async def _governance_loop(self) -> None:
        while self._running:
            try:
                import aiohttp
                async with aiohttp.ClientSession() as session:
                    async with session.get(
                        f"{self.api_url}/api/governance/pressure",
                        timeout=aiohttp.ClientTimeout(total=5),
                    ) as resp:
                        if resp.ok:
                            data = await resp.json()
                            was_shedding = self._shedding_heavy
                            self._shedding_heavy = data.get("write_pressure", False)
                            if self._shedding_heavy and not was_shedding:
                                log.warning("JobEngine: memory pressure — pausing heavy jobs")
                            elif not self._shedding_heavy and was_shedding:
                                log.info("JobEngine: pressure cleared — resuming heavy jobs")
            except Exception:
                pass
            await asyncio.sleep(GOVERNANCE_POLL_INTERVAL)

    async def _schedule_sync_loop(self) -> None:
        while self._running:
            await asyncio.sleep(SCHEDULE_POLL_INTERVAL)
            await self._load_schedules_from_api()
            await self._poll_pending_retries()

    async def _poll_pending_retries(self) -> None:
        """Check /api/ml/jobs/dead-letter and retrigger any jobs that have been reset to pending."""
        try:
            import aiohttp
            async with aiohttp.ClientSession() as session:
                async with session.get(
                    f"{self.api_url}/api/ml/jobs/dead-letter",
                    timeout=aiohttp.ClientTimeout(total=5),
                ) as resp:
                    if not resp.ok:
                        return
                    data = await resp.json()
                    for entry in data.get("dead_letter", []):
                        job_type = entry.get("job_type", "")
                        if job_type in self._job_registry:
                            log.info("JobEngine: retriggering dead-letter job %s", job_type)
                            self.retry_dead_letter(job_type)
        except Exception as exc:
            log.debug("_poll_pending_retries: %s", exc)

    async def _create_job_run_record(self, job_id: str) -> str:
        try:
            import aiohttp
            async with aiohttp.ClientSession() as session:
                async with session.post(
                    f"{self.api_url}/api/ml/jobs",
                    json={
                        "job_type": job_id,
                        "status": "running",
                        "trigger": "scheduler",
                        "started_at_ns": time.time_ns(),
                    },
                    timeout=aiohttp.ClientTimeout(total=5),
                ) as resp:
                    if resp.ok:
                        data = await resp.json()
                        return data.get("id", "")
        except Exception as exc:
            log.debug("Could not create job run record: %s", exc)
        return f"local-{time.time_ns()}"

    async def _patch_job_run_record(
        self,
        run_record_id: str,
        job_id: str,
        status: str,
        error_message: str,
        duration_ms: int,
        result: Any,
    ) -> None:
        payload: dict[str, Any] = {
            "status": status,
            "error_message": error_message,
            "completed_at_ns": time.time_ns(),
            "duration_ms": duration_ms,
        }
        if result and hasattr(result, "__dict__"):
            for k in ("val_auc", "val_f1", "row_count", "quality_passed"):
                v = getattr(result, k, None)
                if v is not None:
                    payload[k] = v
        try:
            import aiohttp
            async with aiohttp.ClientSession() as session:
                async with session.patch(
                    f"{self.api_url}/api/ml/jobs/{run_record_id}",
                    json=payload,
                    timeout=aiohttp.ClientTimeout(total=5),
                ) as resp:
                    pass
        except Exception as exc:
            log.debug("Could not patch job run record: %s", exc)

    def _emit_ml_event(self, event_type: str, payload: dict) -> None:
        try:
            import requests
            requests.post(
                f"{self.api_url}/api/ml/events/publish",
                json={"event_type": event_type, "payload": payload},
                timeout=2,
            )
        except Exception:
            pass

    # ── Metrics ───────────────────────────────────────────────────────────────

    def _start_metrics_server(self) -> None:
        try:
            from prometheus_client import Counter, Histogram, Gauge, start_http_server
            self._metrics = {
                "runs_total": Counter(
                    "bonsai_ml_job_runs_total", "ML job run count", ["job_id", "outcome"]
                ),
                "duration": Histogram(
                    "bonsai_ml_job_duration_seconds", "ML job duration", ["job_id"]
                ),
                "last_success": Gauge(
                    "bonsai_ml_job_last_success_timestamp", "Last success epoch", ["job_id"]
                ),
            }
            start_http_server(METRICS_PORT)
            log.info("JobEngine: Prometheus metrics on :%d/metrics", METRICS_PORT)
        except ImportError:
            log.debug("prometheus_client not installed — metrics disabled")
        except Exception as exc:
            log.debug("Could not start metrics server: %s", exc)

    def _update_metrics_success(self, job_id: str, duration_s: float) -> None:
        if not self._metrics:
            return
        try:
            self._metrics["runs_total"].labels(job_id=job_id, outcome="succeeded").inc()
            self._metrics["duration"].labels(job_id=job_id).observe(duration_s)
            self._metrics["last_success"].labels(job_id=job_id).set(time.time())
        except Exception:
            pass

    def _update_metrics_failure(self, job_id: str) -> None:
        if not self._metrics:
            return
        try:
            self._metrics["runs_total"].labels(job_id=job_id, outcome="failed").inc()
        except Exception:
            pass


class _InMemoryFallbackScheduler:
    """Minimal fallback when APScheduler is not installed. Only supports trigger_job()."""

    def __init__(self, engine: BonsaiJobEngine) -> None:
        self._engine = engine

    async def start_in_background(self) -> None:
        log.warning("JobEngine: APScheduler unavailable — only manual triggers work")

    async def stop(self) -> None:
        pass

    async def add_schedule(self, *args: Any, **kwargs: Any) -> None:
        pass

    async def remove_schedule(self, *args: Any, **kwargs: Any) -> None:
        pass


def _schedule_to_cron(sched: dict) -> str:
    """Convert a DEFAULT_SCHEDULES entry to a crontab string."""
    t = sched.get("trigger", "interval")
    if t == "cron":
        minute = sched.get("minute", 0)
        hour = sched.get("hour", "*")
        dom = "*"
        month = "*"
        dow = sched.get("day_of_week", "*")
        return f"{minute} {hour} {dom} {month} {dow}"
    elif t == "interval":
        seconds = sched.get("seconds", 0)
        minutes = sched.get("minutes", 0)
        hours = sched.get("hours", 0)
        total_minutes = int(seconds / 60 + minutes + hours * 60)
        total_minutes = max(1, total_minutes)
        return f"*/{total_minutes} * * * *"
    return "0 * * * *"


def build_default_engine(api_url: str = DEFAULT_API_URL) -> BonsaiJobEngine:
    """Build and wire a BonsaiJobEngine with all default Bonsai ML jobs registered.

    Imported by collector_engine.py at startup.
    """
    from .export_job import IncrementalExportJob
    from .syslog_embedding_worker import run_syslog_embedding_worker
    from .config_embedding_worker import run_config_embedding_worker
    from .syslog_cluster import SyslogClusterer

    engine = BonsaiJobEngine(api_url=api_url)

    async def anomaly_export(reporter: Any = None) -> Any:
        job = IncrementalExportJob(api_url=api_url)
        return job.run_incremental(export_type="anomaly")

    async def remediation_export(reporter: Any = None) -> Any:
        job = IncrementalExportJob(api_url=api_url)
        return job.run_incremental(export_type="remediation")

    async def syslog_embedding(reporter: Any = None) -> None:
        run_syslog_embedding_worker(api_url=api_url, run_once=True)

    async def config_embedding(reporter: Any = None) -> None:
        run_config_embedding_worker(api_url=api_url, run_once=True)

    async def detection_clustering(reporter: Any = None) -> None:
        clusterer = SyslogClusterer(api_url=api_url)
        clusterer.run()

    engine.register_job("anomaly_export_daily",      anomaly_export,      "cron",     hour=2,  minute=0)
    engine.register_job("remediation_export_weekly", remediation_export,  "cron",     day_of_week=0, hour=2, minute=0)
    engine.register_job("syslog_embedding",          syslog_embedding,    "interval", seconds=60)
    engine.register_job("config_embedding",          config_embedding,    "interval", hours=6)
    engine.register_job("detection_clustering",      detection_clustering, "cron",    day_of_week=0, hour=3, minute=0)

    engine.on_job_success_trigger(
        "anomaly_export_daily",
        "stgnn_training",
        condition_fn=lambda r: r is not None and getattr(r, "quality_passed", False),
    )

    return engine
