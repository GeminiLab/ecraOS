use product::product;

macro_rules! wrap {
    ($first:tt, $second:tt, $label:ident) => {
        ($first, $second, stringify!($label))
    };
}

#[test]
fn expands_a_cartesian_product() {
    assert_eq!(product!([1, 2], [3, 4]), [(1, 3), (1, 4), (2, 3), (2, 4)]);
}

#[test]
fn preserves_the_empty_zero_dimension_result() {
    let result: [(); 0] = product!();
    assert_eq!(result, []);
}

#[test]
fn replaces_every_standalone_at_callback_argument() {
    assert_eq!(
        product!([1, 2] => wrap!(@, @, label)),
        ([(1,), (2,)], [(1,), (2,)], "label")
    );
}

#[test]
fn replaces_at_with_the_empty_zero_dimension_result() {
    macro_rules! inspect {
        ($result:tt, $label:ident) => {
            ($result, stringify!($label))
        };
    }

    let result: ([(); 0], &str) = product!(=> inspect!(@, empty));
    assert_eq!(result, ([], "empty"));
}

#[test]
fn accepts_arbitrary_tokens_inside_callback_arguments() {
    macro_rules! inspect {
        ($result:tt, $type:ty, $expr:expr) => {
            ($result, stringify!($type), $expr)
        };
    }

    assert_eq!(
        product!([1] => inspect!(@, Option<u8>, (2 + 3))),
        ([(1,)], "Option < u8 >", 5)
    );
}

#[test]
fn leaves_at_tokens_inside_larger_arguments_unchanged() {
    macro_rules! inspect {
        ($value:tt) => {
            stringify!($value)
        };
    }

    assert_eq!(product!([1] => inspect!((foo @ bar))), "(foo @ bar)");
}
