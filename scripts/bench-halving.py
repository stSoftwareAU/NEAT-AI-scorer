#!/usr/bin/env python3
"""Freeze and run the opt-in cached-halving protocol. Python standard library only."""

import argparse
import datetime
import hashlib
import json
import os
import pathlib
import platform
import random
import statistics
import subprocess
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parents[1]
ARMS = ["full", "stride", "nested", "cached"]
PROTOCOL = {
    "schema": "neat-halving-protocol/1",
    "paper": "https://arxiv.org/html/2610.37276v1",
    "adaptation": "fixed-population raw-loss replay; no response modelling or evolution",
    "minWinnerRetention": 0.95,
    "maxRelativeRegret": 0.001,
    "minEvaluationSavingVsFull": 0.5,
    "minEvaluationSavingVsNestedExclusive": 0.0,
    "minValidationSeeds": 3,
    "minRepresentativePopulations": 2,
    "timingEvidence": "indicative on a shared host; not a CI gate",
    "uncertainty": "2000 seed-cluster bootstrap means, percentile 95%, RNG seed 680",
}


def encode(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False)


def sha256(path):
    digest = hashlib.sha256()
    with pathlib.Path(path).open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def inventory(directory, suffix):
    """Hash the immediate file set that the native directory loaders accept."""
    directory = pathlib.Path(directory)
    if not directory.is_dir():
        raise ValueError(f"not a directory: {directory}")
    files = sorted(p for p in directory.iterdir() if p.is_file() and p.suffix == suffix)
    if not files:
        raise ValueError(f"no {suffix} files: {directory}")
    return [{"name": p.name, "bytes": p.stat().st_size, "sha256": sha256(p)} for p in files]


def positive_int(value, name):
    if type(value) is not int or value <= 0:
        raise ValueError(f"{name} must be a positive integer")


def validate_config(config):
    if config["schema"] != "neat-halving-config/1":
        raise ValueError("unsupported config schema")
    cases = config["cases"]
    if not cases or {c["split"] for c in cases} != {"calibration", "validation"}:
        raise ValueError("provide calibration and validation cases")
    if len({c["id"] for c in cases}) != len(cases):
        raise ValueError("case IDs must be unique")
    if any(c["kind"] not in ("synthetic", "representative") for c in cases):
        raise ValueError("case kind must be synthetic or representative")
    seeds = config["seeds"]
    if (not seeds or len(set(seeds)) != len(seeds)
            or any(type(s) is not int or not 0 <= s <= 2**32 - 1 for s in seeds)):
        raise ValueError("provide distinct seeds in [0, 2^32-1]")
    repetitions = config["repetitions"]
    positive_int(repetitions, "repetitions")
    if repetitions % 4:
        raise ValueError("repetitions must be a multiple of four for counterbalancing")
    if config["cost"] not in ("MSE", "RMSE", "MAE", "MAPE", "MSLE", "HINGE",
                              "CROSS_ENTROPY", "CATEGORICAL_ERROR"):
        raise ValueError("unsupported canonical cost")
    if not str(config["splitJustification"]).strip():
        raise ValueError("splitJustification must explain cohort separation")
    for key in ("maxCorpusBytes", "maxRecordEvaluations", "trialSeconds", "totalSeconds"):
        positive_int(config["limits"][key], key)
    if config["limits"]["trialSeconds"] > 900:
        raise ValueError("trialSeconds must not exceed 900")
    if len(cases) * len(seeds) * repetitions > 10_000:
        raise ValueError("at most 10000 trials per manifest")


def arm_order(repetition):
    # Williams design: each arm occupies each position, each directed neighbour
    # pair occurs once per four repetitions; no warm-up run is discarded.
    return [ARMS[(index + repetition) % 4] for index in (0, 1, 3, 2)]


def source_identity():
    files = sorted((ROOT / "rust_scorer/src").rglob("*.rs"))
    files += [ROOT / p for p in ("Cargo.toml", "Cargo.lock", "rust_scorer/Cargo.toml",
                                "rust-toolchain.toml", ".cargo/config.toml",
                                "scripts/bench-halving.py")]
    return hashlib.sha256(encode([
        [str(p.relative_to(ROOT)), sha256(p)] for p in files
    ]).encode()).hexdigest()


def hardware():
    cpu_model = platform.processor() or None
    if sys.platform.startswith("linux"):
        with open("/proc/cpuinfo", encoding="utf-8") as source:
            for line in source:
                if line.startswith("model name"):
                    cpu_model = line.partition(":")[2].strip()
                    break
    return {"host": platform.node(), "platform": platform.platform(),
            "machine": platform.machine(), "cpuModel": cpu_model, "logicalCpus": os.cpu_count()}


def environment():
    names = ["NEAT_SCORER_ACTIVATION_THREADS", "NEAT_SCORER_FILE_THREADS",
             "NEAT_SCORER_READ_BYTES", "NEAT_SCORER_GPU_SCRATCH_BYTES",
             "NEAT_SCORER_WORKER_SPLIT", "NEAT_SCORER_IO_MODE", "NEAT_SCORER_GPU",
             "NEAT_SCORER_GPU_INFLIGHT", "RAYON_NUM_THREADS", "RUSTFLAGS",
             "CARGO_ENCODED_RUSTFLAGS"]
    return {n: os.environ[n] for n in sorted(set(names)) if n in os.environ}


def checked_command(argv, seconds=30):
    result = subprocess.run(argv, cwd=ROOT, stdin=subprocess.DEVNULL,
                            capture_output=True, text=True, timeout=seconds, check=False)
    if result.returncode:
        raise ValueError(f"{argv[0]} exited {result.returncode}: {result.stderr.strip()}")
    return result.stdout


def native_protocol(binary):
    value = json.loads(checked_command([str(binary), "--describe-protocol"]))
    if set(value) != {"rates", "eta", "relativeNumericTolerance"}:
        raise ValueError("native protocol omitted or added algorithm fields")
    return value


def prepare(args):
    started = time.monotonic()
    build_secs = None
    if args.binary:
        binary = pathlib.Path(args.binary).resolve(strict=True)
    else:
        build_start = time.monotonic()
        output = checked_command(["cargo", "build", "--locked", "--release", "-p", "rust_scorer",
                                  "--bin", "halving_bench", "--message-format=json"], 900)
        build_secs = time.monotonic() - build_start
        artifacts = [json.loads(line) for line in output.splitlines()]
        binaries = [a["executable"] for a in artifacts if a.get("reason") == "compiler-artifact"
                    and a["target"]["name"] == "halving_bench" and a["executable"]]
        if not binaries:
            raise ValueError("cargo emitted no halving_bench executable")
        binary = pathlib.Path(binaries[-1]).resolve(strict=True)
    if args.synthetic_root:
        config = json.loads(checked_command([str(binary), "--generate-fixture", args.synthetic_root], 60))
        config_root = ROOT
    else:
        config_path = pathlib.Path(args.config).resolve(strict=True)
        config = json.loads(config_path.read_text())
        config_root = config_path.parent
    validate_config(config)
    cases = []
    for case in config["cases"]:
        item = dict(case)
        for directory, suffix, field in (("creaturesDir", ".json", "candidates"),
                                          ("dataDir", ".bin", "data")):
            path = (config_root / case[directory]).resolve(strict=True)
            item[directory] = str(path)
            item[field] = inventory(path, suffix)
        if sum(f["bytes"] for f in item["data"]) > config["limits"]["maxCorpusBytes"]:
            raise ValueError(f"{case['id']}: corpus byte limit exceeded")
        cases.append(item)
    # Hash-level separation cannot prove the absence of duplicate records in
    # different files; the declared provenance must address that limitation.
    for field in ("candidates", "data"):
        hashes = {split: {f["sha256"] for c in cases if c["split"] == split for f in c[field]}
                  for split in ("calibration", "validation")}
        if hashes["calibration"] & hashes["validation"]:
            raise ValueError(f"calibration/validation {field} hashes overlap")
    manifest = {
        "schema": "neat-halving-manifest/1", "protocol": dict(PROTOCOL, **native_protocol(binary)),
        "createdUtc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "config": dict(config, cases=cases), "binary": str(binary), "binarySha256": sha256(binary),
        "sourceSha256": source_identity(),
        "sourceGitHead": checked_command(["git", "rev-parse", "HEAD"]).strip(),
        "scorerVersion": checked_command([str(binary), "--version"]).strip(),
        "rustc": checked_command(["rustc", "--version"]).strip(),
        "neatCoreLockEntry": (ROOT / "Cargo.lock").read_text().split('name = "neat-core"', 1)[1].split("[[package]]", 1)[0].strip(),
        "hardware": hardware(), "environment": environment(),
        "binaryBuildSecs": build_secs, "inputPreparationSecs": config.get("inputPreparationSecs"),
        "inputPreparationIncludedInPrepare": bool(args.synthetic_root),
        "prepareSecs": time.monotonic() - started,
    }
    with open(args.manifest, "x", encoding="utf-8") as target:
        target.write(encode(manifest) + "\n")
    print(f"Frozen manifest: {args.manifest}", file=sys.stderr)


def verify(manifest):
    if manifest["schema"] != "neat-halving-manifest/1":
        raise ValueError("unsupported manifest schema")
    validate_config(manifest["config"])
    if source_identity() != manifest["sourceSha256"] or sha256(manifest["binary"]) != manifest["binarySha256"]:
        raise ValueError("source or binary changed since manifest freeze")
    if manifest["protocol"] != dict(PROTOCOL, **native_protocol(manifest["binary"])):
        raise ValueError("manifest protocol/version differs from this driver")
    if hardware() != manifest["hardware"] or environment() != manifest["environment"]:
        raise ValueError("hardware or tuning environment changed since manifest freeze")
    for case in manifest["config"]["cases"]:
        for path, suffix, field in (("creaturesDir", ".json", "candidates"), ("dataDir", ".bin", "data")):
            if inventory(case[path], suffix) != case[field]:
                raise ValueError(f"{case['id']}: frozen {field} changed")


def distribution(by_seed):
    seed_means = [statistics.mean(by_seed[s]) for s in sorted(by_seed)]
    samples = [x for seed in by_seed.values() for x in seed]
    rng = random.Random(680)
    bootstrap = sorted(statistics.mean(rng.choices(seed_means, k=len(seed_means))) for _ in range(2000))
    return {"pairs": len(samples), "seedMeans": seed_means,
            "mean": statistics.mean(seed_means), "median": statistics.median(samples),
            "min": min(samples), "max": max(samples),
            "bootstrap95": [bootstrap[49], bootstrap[1949]]}


def comparisons(trials, cases):
    result = []
    for case in cases:
        observations = [t for t in trials if t["caseId"] == case["id"] and t["status"] == "ok"]
        if not observations:
            continue
        values = {name: {} for name in ("evaluationSavingVsFull", "evaluationSavingVsNested",
                                        "cachedMinusFullSecs", "cachedMinusStrideSecs", "cachedMinusNestedSecs",
                                        "relativeRegret", "winnerRetention", "nestedControlAgreement")}
        for trial in observations:
            arms = {a["outcome"]["arm"]: a for a in trial["observation"]["arms"]}
            cached, full, nested, stride = (arms[a] for a in ("cachedHalving", "full", "nestedUncached", "independentScreening"))
            count = cached["outcome"]["recordEvaluations"]
            metrics = {
                "evaluationSavingVsFull": 1 - count / full["outcome"]["recordEvaluations"],
                "evaluationSavingVsNested": 1 - count / nested["outcome"]["recordEvaluations"],
                "cachedMinusFullSecs": cached["totalSecs"] - full["totalSecs"],
                "cachedMinusStrideSecs": cached["totalSecs"] - stride["totalSecs"],
                "cachedMinusNestedSecs": cached["totalSecs"] - nested["totalSecs"],
                "relativeRegret": cached["audit"]["regret"] / max(abs(cached["audit"]["bestFullError"]), 1e-12),
                "winnerRetention": float(not cached["audit"]["lostFullWinner"]),
                "nestedControlAgreement": float(cached["outcome"]["eliminatedAt"] == nested["outcome"]["eliminatedAt"]
                                                and cached["outcome"]["selected"] == nested["outcome"]["selected"]),
            }
            for name, value in metrics.items():
                values[name].setdefault(trial["seed"], []).append(value)
        result.append({"caseId": case["id"], "kind": case["kind"], "split": case["split"],
                       "population": len(case["candidates"]),
                       "metrics": {name: distribution(value) for name, value in values.items()}})
    return result


def run(args):
    start = time.monotonic()
    manifest = json.loads(pathlib.Path(args.manifest).read_text())
    verify(manifest)
    config = manifest["config"]
    limits = config["limits"]
    cases = sorted(config["cases"], key=lambda c: (c["split"] != "calibration", c["id"]))
    trials = []
    planned = len(cases) * len(config["seeds"]) * config["repetitions"]
    with open(args.results, "x", encoding="utf-8", buffering=1) as target:
        def emit(value):
            target.write(encode(value) + "\n")
        emit({"type": "manifest", "manifestSha256": sha256(args.manifest), "manifest": manifest})
        for case in cases:
            for seed in config["seeds"]:
                for repetition in range(config["repetitions"]):
                    remaining = limits["totalSeconds"] - (time.monotonic() - start)
                    if remaining <= 0:
                        break
                    order = arm_order(repetition)
                    argv = [manifest["binary"], case["creaturesDir"], case["dataDir"], "--cost", config["cost"],
                            "--seed", str(seed), "--order", ",".join(order),
                            "--max-corpus-bytes", str(limits["maxCorpusBytes"]),
                            "--max-record-evaluations", str(limits["maxRecordEvaluations"])]
                    t = time.monotonic()
                    trial = {"type": "trial", "caseId": case["id"], "seed": seed,
                             "repetition": repetition, "order": order, "status": "failed"}
                    try:
                        child = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True, text=True,
                                               timeout=min(limits["trialSeconds"], remaining), check=False)
                        trial["stderr"] = child.stderr
                        trial["exitCode"] = child.returncode
                        observation = json.loads(child.stdout)
                        if child.returncode != 0 or observation["status"] != "ok":
                            raise ValueError(f"worker failed: {observation}")
                        if observation["schema"] != "neat-halving-observation/1":
                            raise ValueError("unexpected observation schema")
                        trial["status"] = "ok"
                        trial["observation"] = observation
                    except (ValueError, KeyError, subprocess.TimeoutExpired) as error:
                        trial["error"] = str(error)
                        trial["completedRecordEvaluations"] = None
                    trial["wallSecs"] = time.monotonic() - t
                    trials.append(trial)
                    emit(trial)
        integrity_error = None
        try:
            verify(manifest)
        except (ValueError, OSError, KeyError) as error:
            integrity_error = str(error)
        paired = comparisons(trials, cases)
        successes = sum(t["status"] == "ok" for t in trials)
        representative = [p for p in paired if p["split"] == "validation" and p["kind"] == "representative"]
        complete = successes == planned and integrity_error is None
        eligible = (len({p["population"] for p in representative}) >= PROTOCOL["minRepresentativePopulations"]
                    and len(config["seeds"]) >= PROTOCOL["minValidationSeeds"])
        quality = all(p["metrics"]["winnerRetention"]["mean"] >= PROTOCOL["minWinnerRetention"]
                      and p["metrics"]["relativeRegret"]["max"] <= PROTOCOL["maxRelativeRegret"]
                      and p["metrics"]["evaluationSavingVsFull"]["min"] >= PROTOCOL["minEvaluationSavingVsFull"]
                      and p["metrics"]["evaluationSavingVsNested"]["min"] > PROTOCOL["minEvaluationSavingVsNestedExclusive"]
                      and p["metrics"]["nestedControlAgreement"]["min"] == 1 for p in representative)
        timing = all(p["metrics"]["cachedMinusStrideSecs"]["bootstrap95"][1] < 0
                     and p["metrics"]["cachedMinusNestedSecs"]["bootstrap95"][1] < 0 for p in representative)
        verdict = ("failed-or-incomplete" if not complete else "inconclusive-no-representative-validation" if not eligible
                   else "selection-or-saving-threshold-not-met" if not quality else "inconclusive-timing" if not timing
                   else "indicative-benefit-requires-quiet-host-confirmation")
        summary = {"type": "summary", "schema": "neat-halving-results/1", "verdict": verdict,
                   "coverage": {"plannedTrials": planned, "successfulTrials": successes,
                                "failedTrials": len(trials) - successes, "notRunTrials": planned - len(trials)},
                   "integrityError": integrity_error, "comparisons": paired,
                   "knownRecordEvaluations": sum(t["observation"]["totalRecordEvaluations"] for t in trials if t["status"] == "ok"),
                   "failedTrialEvaluationCountsKnown": not any(t["status"] != "ok" for t in trials),
                   "runWallSecs": time.monotonic() - start,
                   "preparationWallSecs": manifest["prepareSecs"],
                   "inputPreparationSecs": manifest["inputPreparationSecs"],
                   "inputPreparationIncludedInPrepare": manifest["inputPreparationIncludedInPrepare"],
                   "binaryBuildSecs": manifest["binaryBuildSecs"]}
        emit(summary)
    print(f"{verdict}; {successes}/{planned} trials; results: {args.results}", file=sys.stderr)
    return 0 if complete else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    freeze = commands.add_parser("prepare", help="freeze hashes, protocol and compute limits before validation")
    sources = freeze.add_mutually_exclusive_group(required=True)
    sources.add_argument("--config")
    sources.add_argument("--synthetic-root")
    freeze.add_argument("--binary", help="use a prebuilt worker; binary build cost is recorded as unknown")
    freeze.add_argument("--manifest", required=True)
    execute = commands.add_parser("run", help="verify the frozen manifest and execute paired trials")
    execute.add_argument("--manifest", required=True)
    execute.add_argument("--results", required=True)
    args = parser.parse_args()
    try:
        if args.command == "prepare":
            prepare(args)
            return 0
        return run(args)
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(f"halving experiment failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
