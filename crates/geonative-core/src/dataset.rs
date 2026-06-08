//! Object-safe `Dataset` + `Layer` traits — the polymorphic surface that
//! every concrete reader (geonative-filegdb, geonative-shapefile, future
//! GPKG / GeoJSON / FlatGeoBuf) implements.
//!
//! ## When to use traits vs concrete types
//!
//! - **Concrete types** (`geonative_filegdb::Geodatabase`,
//!   `geonative_shapefile::Shapefile`, …) are zero-cost — no virtual
//!   dispatch, no boxing. Use them when your code knows the format up front.
//! - **Traits** (`Dataset`, `Layer`) enable format-polymorphic code: a
//!   converter that opens any source and writes to any sink, a CLI that
//!   probes by extension, etc. The cost is `Box<dyn>` (one indirection per
//!   call).
//!
//! ## Single-layer formats
//!
//! Shapefile-style formats have no concept of multiple layers per file. The
//! [`SingleLayerDataset`] adapter wraps any [`Layer`] so it can stand in for
//! a `Dataset` (with a single sentinel layer name, default `"default"`).

use std::sync::Arc;

use crate::{Error, Feature, Result, Schema};

/// A container of one or more named layers — the polymorphic equivalent of
/// `geonative_filegdb::Geodatabase`.
pub trait Dataset {
    /// Names of the user-facing layers in deterministic order.
    fn layer_names(&self) -> Vec<String>;

    /// Open the layer with the given name. Returns
    /// [`Error::LayerNotFound`] if the name isn't present.
    fn open_layer<'a>(&'a self, name: &str) -> Result<Box<dyn Layer + 'a>>;
}

/// A stream of features sharing one schema — the polymorphic equivalent of
/// `geonative_filegdb::Layer`, `geonative_shapefile::Shapefile`, etc.
pub trait Layer {
    /// Human-friendly layer name (single-layer formats: `"default"`).
    fn name(&self) -> &str;

    /// The schema of this layer's features.
    fn schema(&self) -> &Schema;

    /// Declared feature count, if the format exposes it cheaply. Returns
    /// `None` if computing the count would require a full scan.
    fn feature_count(&self) -> Option<i64>;

    /// Lazy feature iterator. Each call starts a fresh pass over the layer.
    fn read<'a>(&'a self) -> Box<dyn Iterator<Item = Result<Feature>> + 'a>;
}

/// Adapter that wraps any [`Layer`] in a [`Dataset`] facade with one
/// sentinel layer name. Use this when a single-layer format like Shapefile
/// needs to satisfy a `Dataset` interface.
#[derive(Debug)]
pub struct SingleLayerDataset<L: Layer> {
    /// The wrapped layer. `Arc` so the adapter is cheap to clone and the
    /// closure inside `open_layer` can hand out a borrowed reference.
    pub inner: Arc<L>,
    /// The synthetic layer name surfaced via [`Dataset::layer_names`].
    /// Default `"default"`.
    pub name: String,
}

impl<L: Layer> SingleLayerDataset<L> {
    pub fn new(layer: L) -> Self {
        Self {
            inner: Arc::new(layer),
            name: "default".to_string(),
        }
    }

    pub fn with_name(layer: L, name: impl Into<String>) -> Self {
        Self {
            inner: Arc::new(layer),
            name: name.into(),
        }
    }
}

impl<L: Layer + 'static> Dataset for SingleLayerDataset<L> {
    fn layer_names(&self) -> Vec<String> {
        vec![self.name.clone()]
    }

    fn open_layer<'a>(&'a self, name: &str) -> Result<Box<dyn Layer + 'a>> {
        if name == self.name {
            Ok(Box::new(LayerRef(self.inner.as_ref())))
        } else {
            Err(Error::LayerNotFound(name.to_string()))
        }
    }
}

/// Thin wrapper so we can hand out a `Box<dyn Layer>` borrowing from an `Arc<L>`.
#[derive(Debug)]
struct LayerRef<'a, L: Layer>(&'a L);

impl<'a, L: Layer> Layer for LayerRef<'a, L> {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn schema(&self) -> &Schema {
        self.0.schema()
    }
    fn feature_count(&self) -> Option<i64> {
        self.0.feature_count()
    }
    fn read<'b>(&'b self) -> Box<dyn Iterator<Item = Result<Feature>> + 'b> {
        self.0.read()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Crs, FieldDef, GeomField, GeometryType, ValueType};

    /// A trivial in-memory Layer for trait testing.
    struct TestLayer {
        name: String,
        schema: Schema,
        features: Vec<Feature>,
    }

    impl Layer for TestLayer {
        fn name(&self) -> &str {
            &self.name
        }
        fn schema(&self) -> &Schema {
            &self.schema
        }
        fn feature_count(&self) -> Option<i64> {
            Some(self.features.len() as i64)
        }
        fn read<'a>(&'a self) -> Box<dyn Iterator<Item = Result<Feature>> + 'a> {
            Box::new(self.features.iter().cloned().map(Ok))
        }
    }

    fn make_layer() -> TestLayer {
        TestLayer {
            name: "roads".into(),
            schema: Schema::new(
                vec![FieldDef::new("id", ValueType::Int32, false)],
                Some(GeomField::new("geom", GeometryType::Point)),
                Crs::Unknown,
            ),
            features: vec![
                Feature::new(Some(1), None, vec![crate::Value::Int32(1)]),
                Feature::new(Some(2), None, vec![crate::Value::Int32(2)]),
            ],
        }
    }

    #[test]
    fn single_layer_dataset_exposes_one_layer() {
        let ds = SingleLayerDataset::with_name(make_layer(), "default");
        assert_eq!(ds.layer_names(), vec!["default"]);
        let layer = ds.open_layer("default").unwrap();
        assert_eq!(layer.feature_count(), Some(2));
    }

    #[test]
    fn single_layer_dataset_rejects_wrong_name() {
        let ds = SingleLayerDataset::new(make_layer());
        assert!(ds.open_layer("nope").is_err());
    }

    #[test]
    fn layer_read_iterator_yields_features() {
        let layer = make_layer();
        let v: Vec<_> = layer.read().collect::<Result<Vec<_>>>().unwrap();
        assert_eq!(v.len(), 2);
    }

    #[test]
    fn dataset_is_object_safe() {
        // Compile-time check: Box<dyn Dataset> is valid.
        fn _take_dataset(_: Box<dyn Dataset>) {}
        let ds: Box<dyn Dataset> = Box::new(SingleLayerDataset::new(make_layer()));
        _take_dataset(ds);
    }
}
