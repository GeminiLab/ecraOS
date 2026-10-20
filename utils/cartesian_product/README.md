# `cartesian_product`

`cartesian_product` provides the `product!` procedural macro. It expands literal dimensions into
an array of tuples at compile time and requires no runtime support.

## Usage

Add the crate as a dependency and import the macro:

```toml
[dependencies]
cartesian_product = "0.1"
```

```rust
use cartesian_product::product;

let pairs: [(u8, u8); 4] = product!([1, 2], [10, 20]);
assert_eq!(pairs, [(1, 10), (1, 20), (2, 10), (2, 20)]);
```

Each bracketed list is a dimension. The macro emits one tuple for every combination, preserving
dimension order and varying the last dimension fastest. A trailing comma is allowed. One dimension
produces one-element tuples:

```rust
use cartesian_product::product;

let values: [(u8,); 2] = product!([7, 8]);
assert_eq!(values, [(7,), (8,)]);
```

With no dimensions, or with an empty dimension, the macro emits an empty array. Add a type
annotation when Rust cannot infer the element type:

```rust
use cartesian_product::product;

let empty: [(); 0] = product!();
assert_eq!(empty, []);
```

## Callback macros

Append `=> callback!(...)` to pass the generated array to a macro. Each callback argument that
consists only of `@` is replaced with the array. Other arguments pass through unchanged:

```rust
use cartesian_product::product;

macro_rules! capture {
    ($values:tt, $label:ident) => {
        ($values, stringify!($label))
    };
}

let result = product!([1, 2] => capture!(@, pairs));
assert_eq!(result, ([(1,), (2,)], "pairs"));
```

The callback name is a single macro identifier. Its arguments are comma-separated and may contain
nested token groups. Only an argument that is exactly `@` is substituted, so an `@` inside a larger
argument remains unchanged.
