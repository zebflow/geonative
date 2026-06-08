# geonative-mvt

Mapbox Vector Tile (MVT 2.1) encoder for the [geonative](https://geonative.zebflow.com) geospatial library.

Hand-rolled protobuf (no `prost` / `protobuf` dependency), consumes any `geonative_core::Feature` stream, emits standard `.mvt` byte payloads consumed by Mapbox GL JS, MapLibre, OpenLayers, tile servers, etc.
