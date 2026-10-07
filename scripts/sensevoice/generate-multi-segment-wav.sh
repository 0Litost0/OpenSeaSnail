#!/usr/bin/env bash
# 从项目内的标准 PCM WAV 夹具生成「语音—静音—语音」多段 WAV；仅供 server PoC。
set -euo pipefail

usage() { printf '%s\n' "Usage: $0 --input PCM_WAV --output WAV [--silence-ms N]"; }
die() { printf '%s\n' "error: $*" >&2; exit 1; }

INPUT=""
OUTPUT=""
SILENCE_MS=1500
while [[ $# -gt 0 ]]; do
  case "$1" in
    --input) [[ $# -ge 2 ]] || die "--input requires a path"; INPUT="$2"; shift 2 ;;
    --output) [[ $# -ge 2 ]] || die "--output requires a path"; OUTPUT="$2"; shift 2 ;;
    --silence-ms) [[ $# -ge 2 && "$2" =~ ^[1-9][0-9]*$ ]] || die "--silence-ms requires a positive integer"; SILENCE_MS="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done
[[ -n "$INPUT" && -n "$OUTPUT" ]] || die "--input and --output are required"
command -v perl >/dev/null 2>&1 || die "perl is required to generate the temporary PCM WAV fixture"
[[ -f "$INPUT" && ! -e "$OUTPUT" ]] || die "input missing or refusing to overwrite output"
OUTPUT_DIR="$(dirname "$OUTPUT")"
[[ -d "$OUTPUT_DIR" ]] || die "output parent directory does not exist: $OUTPUT_DIR"
TEMPORARY="$(mktemp "$OUTPUT_DIR/.seasnail-multi-wav.XXXXXX")"
cleanup() { rm -f "$TEMPORARY"; }
trap cleanup EXIT

perl - "$INPUT" "$TEMPORARY" "$SILENCE_MS" <<'PERL'
use strict;
use warnings;
my ($input, $output, $silence_ms) = @ARGV;
open my $in, '<:raw', $input or die "open input: $!";
read($in, my $header, 44) == 44 or die "input is not a 44-byte-header WAV";
my ($riff, $riff_size, $wave, $fmt, $fmt_size, $format, $channels, $rate, $byte_rate, $align, $bits, $data, $data_size) =
  unpack('a4Va4a4VvvVVvva4V', $header);
die "input must be PCM WAVE" unless $riff eq 'RIFF' && $wave eq 'WAVE' && $fmt eq 'fmt ' && $fmt_size == 16 && $format == 1 && $data eq 'data';
die "invalid PCM parameters" unless $channels > 0 && $rate > 0 && $align > 0 && $bits == 16 && $byte_rate == $rate * $align;
read($in, my $pcm, $data_size) == $data_size or die "truncated PCM data";
my $silence_bytes = int($byte_rate * $silence_ms / 1000);
$silence_bytes -= $silence_bytes % $align;
my $payload = $pcm . ("\0" x $silence_bytes) . $pcm;
my $out_header = pack('a4Va4a4VvvVVvva4V', 'RIFF', 36 + length($payload), 'WAVE', 'fmt ', 16, 1, $channels, $rate, $byte_rate, $align, $bits, 'data', length($payload));
open my $out, '>:raw', $output or die "open output: $!";
print {$out} $out_header, $payload or die "write output: $!";
PERL

# BSD `ln -h` provides an atomic no-clobber publish in the target directory and
# refuses to follow a destination symlink. A concurrent creator wins safely.
if ! ln -h "$TEMPORARY" "$OUTPUT" 2>/dev/null; then
  die "output appeared during generation; refusing to overwrite: $OUTPUT"
fi
rm -f "$TEMPORARY"
trap - EXIT
