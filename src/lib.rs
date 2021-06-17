pub mod gas;

pub mod turfs;

use auxtools::*;

use gas::*;

use reaction::react_by_id;

use constants::*;

#[hook("/proc/process_atmos_callbacks")]
fn _atmos_callback_handle() {
	auxcallback::callback_processing_hook(args)
}

#[hook("/datum/gas_mixture/proc/__gasmixture_register")]
fn _register_gasmixture_hook() {
	GasMixtures::register_gasmix(src)
}

#[hook("/datum/gas_mixture/proc/__gasmixture_unregister")]
fn _unregister_gasmixture_hook() {
	GasMixtures::unregister_gasmix(src)
}

#[hook("/datum/gas_mixture/proc/heat_capacity")]
fn _heat_cap_hook() {
	with_mix(src, |mix| Ok(Value::from(mix.heat_capacity())))
}

#[hook("/datum/gas_mixture/proc/set_min_heat_capacity")]
fn _min_heat_cap_hook() {
	if args.is_empty() {
		Err(runtime!(
			"attempted to set min heat capacity with no argument"
		))
	} else {
		with_mix_mut(src, |mix| {
			mix.set_min_heat_capacity(args[0].as_number().unwrap_or_default());
			Ok(Value::null())
		})
	}
}

#[hook("/datum/gas_mixture/proc/total_moles")]
fn _total_moles_hook() {
	with_mix(src, |mix| Ok(Value::from(mix.total_moles())))
}

#[hook("/datum/gas_mixture/proc/return_pressure")]
fn _return_pressure_hook() {
	with_mix(src, |mix| Ok(Value::from(mix.return_pressure())))
}

#[hook("/datum/gas_mixture/proc/return_temperature")]
fn _return_temperature_hook() {
	with_mix(src, |mix| Ok(Value::from(mix.get_temperature())))
}

#[hook("/datum/gas_mixture/proc/return_volume")]
fn _return_volume_hook() {
	with_mix(src, |mix| Ok(Value::from(mix.volume)))
}

#[hook("/datum/gas_mixture/proc/thermal_energy")]
fn _thermal_energy_hook() {
	with_mix(src, |mix| Ok(Value::from(mix.thermal_energy())))
}

#[hook("/datum/gas_mixture/proc/merge")]
fn _merge_hook() {
	if args.is_empty() {
		Err(runtime!("Tried merging nothing into a gas mixture"))
	} else {
		with_mixes_mut(src, &args[0], |src_mix, giver_mix| {
			src_mix.merge(giver_mix);
			Ok(Value::null())
		})
	}
}

#[hook("/datum/gas_mixture/proc/__remove_ratio")]
fn _remove_ratio_hook() {
	if args.len() < 2 {
		Err(runtime!("remove_ratio called with fewer than 2 arguments"))
	} else {
		with_mixes_mut(src, &args[0], |src_mix, into_mix| {
			src_mix.remove_ratio_into(args[1].as_number().unwrap_or_default(), into_mix);
			Ok(Value::null())
		})
	}
}

#[hook("/datum/gas_mixture/proc/__remove")]
fn _remove_hook() {
	if args.len() < 2 {
		Err(runtime!("remove called with fewer than 2 arguments"))
	} else {
		with_mixes_mut(src, &args[0], |src_mix, into_mix| {
			src_mix.remove_into(args[1].as_number().unwrap_or_default(), into_mix);
			Ok(Value::null())
		})
	}
}

#[hook("/datum/gas_mixture/proc/copy_from")]
fn _copy_from_hook() {
	if args.is_empty() {
		Err(runtime!("Tried copying a gas mix from nothing"))
	} else {
		with_mixes_mut(src, &args[0], |src_mix, giver_mix| {
			src_mix.copy_from_mutable(giver_mix);
			Ok(Value::null())
		})
	}
}

#[hook("/datum/gas_mixture/proc/temperature_share")]
fn _temperature_share_hook() {
	let arg_num = args.len();
	match arg_num {
		2 => with_mixes_mut(src, &args[0], |src_mix, share_mix| {
			Ok(Value::from(src_mix.temperature_share(
				share_mix,
				args[1].as_number().unwrap_or_default(),
			)))
		}),
		4 => with_mix_mut(src, |mix| {
			Ok(Value::from(mix.temperature_share_non_gas(
				args[0].as_number().unwrap_or_default(),
				args[1].as_number().unwrap_or_default(),
				args[2].as_number().unwrap_or_default(),
			)))
		}),
		_ => Err(runtime!("Invalid args for temperature_share")),
	}
}

#[hook("/datum/gas_mixture/proc/get_gases")]
fn _get_gases_hook() {
	with_mix(src, |mix| {
		let gases_list: List = List::new();
		with_gas_info(|gas_info| {
			mix.with_gases(|gases| {
				for (i, amt) in gases.iter().copied().enumerate() {
					if amt > GAS_MIN_MOLES {
						gases_list.append(Value::from_string(
							gas_info
								.get(i)
								.ok_or_else(|| runtime!("Invalid gas index: {}", i))?
								.id,
						)?);
					}
				}
				Ok(())
			})
		})?;
		Ok(Value::from(gases_list))
	})
}

#[hook("/datum/gas_mixture/proc/set_temperature")]
fn _set_temperature_hook() {
	let v = args
		.get(0)
		.ok_or_else(|| runtime!("Wrong amount of arguments for set_temperature: 0!"))?
		.as_number()
		.map_err(|_| {
			runtime!(
				"Attempt to interpret non-number value as number {} {}:{}",
				std::file!(),
				std::line!(),
				std::column!()
			)
		})?;
	if !v.is_finite() {
		Err(runtime!(
			"Attempted to set a temperature to a number that is NaN or infinite."
		))
	} else {
		with_mix_mut(src, |mix| {
			mix.set_temperature(v.max(2.7));
			Ok(Value::null())
		})
	}
}

#[hook("/datum/gas_mixture/proc/partial_heat_capacity")]
fn _partial_heat_capacity() {
	if args.is_empty() {
		Err(runtime!("Incorrect arg len for partial_heat_capacity (0)."))
	} else {
		with_mix(src, |mix| {
			Ok(Value::from(
				mix.partial_heat_capacity(gas_idx_from_value(&args[0])?),
			))
		})
	}
}

#[hook("/datum/gas_mixture/proc/set_volume")]
fn _set_volume_hook() {
	if args.is_empty() {
		Err(runtime!("Attempted to set volume to nothing."))
	} else {
		with_mix_mut(src, |mix| {
			mix.volume = args[0].as_number().map_err(|_| {
				runtime!(
					"Attempt to interpret non-number value as number {} {}:{}",
					std::file!(),
					std::line!(),
					std::column!()
				)
			})?;
			Ok(Value::null())
		})
	}
}

#[hook("/datum/gas_mixture/proc/get_moles")]
fn _get_moles_hook() {
	if args.is_empty() {
		Err(runtime!("Incorrect arg len for get_moles (0)."))
	} else {
		with_mix(src, |mix| {
			Ok(Value::from(mix.get_moles(gas_idx_from_value(&args[0])?)))
		})
	}
}

#[hook("/datum/gas_mixture/proc/set_moles")]
fn _set_moles_hook() {
	if args.len() < 2 {
		return Err(runtime!("Incorrect arg len for set_moles (less than 2)."));
	}
	let vf = args[1].as_number().unwrap_or_default();
	if !vf.is_finite() {
		return Err(runtime!("Attempted to set moles to NaN or infinity."));
	}
	if vf < 0.0 {
		return Err(runtime!("Attempted to set moles to a negative number."));
	}
	with_mix_mut(src, |mix| {
		mix.set_moles(gas_idx_from_value(&args[0])?, vf);
		Ok(Value::null())
	})
}

#[hook("/datum/gas_mixture/proc/adjust_moles")]
fn _adjust_moles_hook(id_val: Value, num_val: Value) {
	let vf = num_val.as_number().unwrap_or_default();
	with_mix_mut(src, |mix| {
		mix.adjust_moles(gas_idx_from_value(&id_val)?, vf);
		Ok(Value::null())
	})
}

#[hook("/datum/gas_mixture/proc/scrub_into")]
fn _scrub_into_hook(into: Value, ratio_v: Value, gas_list: Value) {
	let ratio = ratio_v.as_number().map_err(|_| {
		runtime!(
			"Attempt to interpret non-number value as number {} {}:{}",
			std::file!(),
			std::line!(),
			std::column!()
		)
	})?;
	let gases_to_scrub = gas_list.as_list().map_err(|_| {
		runtime!(
			"Attempt to interpret non-list value as list {} {}:{}",
			std::file!(),
			std::line!(),
			std::column!()
		)
	})?;
	GasMixtures::with_all_mixtures(|all_mixtures| {
		let mut src_gas = all_mixtures
			.get(value_to_idx(src)?)
			.ok_or_else(|| {
				runtime!(
					"No gas mixture with ID {} exists!",
					value_to_idx(src).unwrap()
				)
			})?
			.write();
		let mut dest_gas = all_mixtures
			.get(value_to_idx(into)?)
			.ok_or_else(|| {
				runtime!(
					"No gas mixture with ID {} exists!",
					value_to_idx(into).unwrap()
				)
			})?
			.write();
		GasMixtures::with_new_mix_and_lock(src_gas.volume, all_mixtures, |removed| {
			src_gas.remove_ratio_into(ratio, removed);
			GasMixtures::with_new_mix_and_lock(src_gas.volume, all_mixtures, |buffer| {
				buffer.set_temperature(src_gas.get_temperature());
				for idx in 1..gases_to_scrub.len() + 1 {
					if let Ok(gas_id) = gas_idx_from_value(&gases_to_scrub.get(idx).unwrap()) {
						buffer.set_moles(gas_id, removed.get_moles(gas_id));
						removed.set_moles(gas_id, 0.0);
					}
				}
				dest_gas.merge(&buffer);
				src_gas.merge(&removed);
				Ok(Value::from(true))
			})
		})
	})
}

#[hook("/datum/gas_mixture/proc/mark_immutable")]
fn _mark_immutable_hook() {
	with_mix_mut(src, |mix| {
		mix.mark_immutable();
		Ok(Value::null())
	})
}

#[hook("/datum/gas_mixture/proc/clear")]
fn _clear_hook() {
	with_mix_mut(src, |mix| {
		mix.clear();
		Ok(Value::null())
	})
}

#[hook("/datum/gas_mixture/proc/compare")]
fn _compare_hook() {
	if args.is_empty() {
		Err(runtime!("Tried comparing a gas mix to nothing"))
	} else {
		with_mixes(src, &args[0], |gas_one, gas_two| {
			Ok(Value::from(
				gas_one.temperature_compare(gas_two)
					|| gas_one.compare_with(gas_two, MINIMUM_MOLES_DELTA_TO_MOVE),
			))
		})
	}
}

#[hook("/datum/gas_mixture/proc/multiply")]
fn _multiply_hook() {
	with_mix_mut(src, |mix| {
		mix.multiply(
			args.get(0)
				.map_or(0.0, |arg| arg.as_number().unwrap_or(1.0)),
		);
		Ok(Value::null())
	})
}

#[hook("/datum/gas_mixture/proc/react")]
fn _react_hook(holder: Value) {
	let mut ret: i32 = 0;
	if let Some(reactions) = with_mix(src, |mix| Ok(mix.all_reactable()))? {
		for reaction in reactions.iter().copied() {
			ret |= react_by_id(reaction, src, holder)?
				.as_number()
				.unwrap_or_default() as i32;
			if ret & STOP_REACTIONS == STOP_REACTIONS {
				return Ok(Value::from(ret as f32));
			}
		}
	}
	Ok(Value::from(ret as f32))
}

#[hook("/datum/gas_mixture/proc/adjust_heat")]
fn _adjust_heat_hook(heat: Value) {
	with_mix_mut(src, |mix| {
		mix.adjust_heat(heat.as_number().unwrap_or_default());
		Ok(Value::null())
	})
}

#[hook("/datum/gas_mixture/proc/transfer_to")]
fn _transfer_hook(other: Value, moles: Value) {
	with_mixes_mut(src, other, |our_mix, other_mix| {
		other_mix.merge(&our_mix.remove(moles.as_number().map_err(|_| {
			runtime!(
				"Attempt to interpret non-number value as number {} {}:{}",
				std::file!(),
				std::line!(),
				std::column!()
			)
		})?));
		Ok(Value::null())
	})
}

#[hook("/datum/gas_mixture/proc/transfer_ratio_to")]
fn _transfer_ratio_hook(other: Value, ratio: Value) {
	with_mixes_mut(src, other, |our_mix, other_mix| {
		other_mix.merge(&our_mix.remove_ratio(ratio.as_number().map_err(|_| {
			runtime!(
				"Attempt to interpret non-number value as number {} {}:{}",
				std::file!(),
				std::line!(),
				std::column!()
			)
		})?));
		Ok(Value::null())
	})
}

#[hook("/datum/gas_mixture/proc/equalize_with")]
fn _equalize_with_hook() {
	with_mixes_custom(
		src,
		args.get(0)
			.ok_or_else(|| runtime!("Wrong number of args for equalize_with: 0"))?,
		|src_lock, total_lock| {
			let src_gas = &mut src_lock.write();
			let vol = src_gas.volume;
			let total_gas = total_lock.read();
			src_gas.copy_from_mutable(&total_gas);
			src_gas.multiply(vol / total_gas.volume);
			Ok(Value::null())
		},
	)
}

#[hook("/proc/equalize_all_gases_in_list")]
fn _equalize_all_hook() {
	use std::collections::BTreeSet;
	let value_list = args
		.get(0)
		.ok_or_else(|| runtime!("Wrong number of args for equalize all: 0"))?
		.as_list()
		.map_err(|_| {
			runtime!(
				"Attempt to interpret non-list value as list {} {}:{}",
				std::file!(),
				std::line!(),
				std::column!()
			)
		})?;
	let gas_list: BTreeSet<usize> = (1..value_list.len() + 1)
		.map(|i| {
			value_list
				.get(i)
				.unwrap()
				.get_number(byond_string!("_extools_pointer_gasmixture"))
				.unwrap()
				.to_bits() as usize
		})
		.collect(); // collect because get_number is way slower than the one-time allocation
	GasMixtures::with_all_mixtures(move |all_mixtures| {
		GasMixtures::with_new_mix_and_lock(0.0, all_mixtures, |tot| {
			let mut tot_vol: f64 = 0.0;
			for &id in gas_list.iter() {
				if let Some(src_gas_lock) = all_mixtures.get(id) {
					let src_gas = src_gas_lock.read();
					tot.merge(&src_gas);
					tot_vol += src_gas.volume as f64;
				}
			}
			if tot_vol > 0.0 {
				for &id in gas_list.iter() {
					if let Some(dest_gas_lock) = all_mixtures.get(id) {
						let dest_gas = &mut dest_gas_lock.write();
						let vol = dest_gas.volume; // don't wanna borrow it in the below
						dest_gas.copy_from_mutable(&tot);
						dest_gas.multiply((vol as f64 / tot_vol) as f32);
					}
				}
			}
		})
	});
	Ok(Value::null())
}

#[hook("/datum/controller/subsystem/air/proc/get_amt_gas_mixes")]
fn _hook_amt_gas_mixes() {
	Ok(Value::from(amt_gases() as f32))
}

#[hook("/datum/controller/subsystem/air/proc/get_max_gas_mixes")]
fn _hook_max_gas_mixes() {
	Ok(Value::from(tot_gases() as f32))
}
