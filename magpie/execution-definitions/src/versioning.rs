use std::collections::HashMap;

use crate::activation::{ObjectArgument, ResolvedNandoArgument};

pub fn merge_version_constraints(objects: &Vec<ResolvedNandoArgument>) {
    if objects.is_empty() {
        return;
    }

    // Union the constraints of all mutable objects, maintaining the max observed version of any
    // overlapping foreign objects.
    let constraint_universe = objects.iter().fold(
        HashMap::with_capacity(objects.len()),
        |mut acc, resolved_obj| match resolved_obj {
            ResolvedNandoArgument::Object(ref oa) => match oa {
                ObjectArgument::RWObject(obj) => {
                    obj.get_constraint_set().union_as_map(&acc, |a, b| {
                        let e = std::cmp::max_by_key(a, b, |e| e.1);
                        (e.0, e.1)
                    })
                }
                ObjectArgument::ROObject(obj) => {
                    acc.entry(obj.get_id())
                        .and_modify(|v| *v = std::cmp::max(*v, obj.get_version()))
                        .or_insert(obj.get_version());
                    acc
                }
                ObjectArgument::UnresolvedObject(_) => unreachable!(),
            },
            _ => acc,
        },
    );

    // Update the constraint set with the modified object versions.
    for object in objects {
        let Some(ObjectArgument::RWObject(object)) = object.get_inner_object_argument() else {
            continue;
        };

        let object_id = object.id;
        // FIXME can we avoid doing this element-wise?
        for constraint in &constraint_universe {
            if *constraint.0 == object_id {
                continue;
            }

            object.upsert_version_constraint(*constraint.0, *constraint.1);
        }
    }
}
