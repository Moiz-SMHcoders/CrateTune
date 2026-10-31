# Video fixtures

`clip.mp4`: 3 s of ffmpeg's `testsrc2` pattern, 128×72, 10 fps, H.264 Main with B-frames,
fragmented like YouTube's DASH video-only formats (`ftyp`, `moov` with `mvex`, one `sidx`, a
`moof` + `mdat` per second starting with a keyframe). Bytes 0–730 are the init segment, 731–806
the index. Made with ffmpeg 8.1.2:

```sh
ffmpeg -f lavfi -i testsrc2=size=128x72:rate=10:duration=3 -c:v libx264 -profile:v main \
  -bf 2 -g 10 -keyint_min 10 -sc_threshold 0 -crf 32 -pix_fmt yuv420p -an -map_metadata -1 \
  -fflags +bitexact -flags:v +bitexact \
  -movflags +frag_keyframe+empty_moov+default_base_moof+global_sidx+negative_cts_offsets \
  -f mp4 clip.mp4
```
