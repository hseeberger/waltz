#[macro_export]
macro_rules! init {
    ($ctx:ident, $s:expr) => {
        |$ctx| async {
            let state = $s;
            ($ctx, state)
        }
    };
}
