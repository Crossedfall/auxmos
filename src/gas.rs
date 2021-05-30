pub mod constants;
pub mod gas_mixture;
pub mod reaction;

#[cfg(feature = "reaction_hooks")]
pub mod reaction_hooks;

use auxtools::*;

use std::collections::{BTreeMap, HashMap};

use gas_mixture::GasMixture;

use parking_lot::{const_rwlock, RwLock};

use std::sync::atomic::{AtomicUsize, Ordering};

use std::cell::RefCell;

use reaction::Reaction;

static TOTAL_NUM_GASES: AtomicUsize = AtomicUsize::new(0);

static GAS_SPECIFIC_HEAT: RwLock<Option<Vec<f32>>> = const_rwlock(None);

static GAS_VIS_THRESHOLD: RwLock<Option<Vec<Option<f32>>>> = const_rwlock(None); // the things we do for globals

static REACTION_INFO: RwLock<Option<Vec<Reaction>>> = const_rwlock(None);

#[derive(Default)]
struct GasIDInfo {
	id_to_type: Vec<Value>,
	id_from_type: BTreeMap<u32, usize>,
	id_from_string: HashMap<std::string::String, usize>,
}

thread_local! {
	static GAS_ID_INFO: RefCell<GasIDInfo> = RefCell::new(Default::default());
	#[cfg(feature = "reaction_hooks")]
	static FUSION_POWER: RefCell<Vec<f32>> = RefCell::new(Vec::new())
}

#[hook("/proc/auxtools_atmos_init")]
fn _hook_init() {
	*REACTION_INFO.write() = Some(get_reaction_info());
	let gas_types_list: auxtools::List = Proc::find("/proc/gas_types")
		.ok_or_else(|| runtime!("Could not find gas_types!"))?
		.call(&[])?
		.as_list()?;
	GAS_ID_INFO.with(|g_| {
		let mut gas_id_info = g_.borrow_mut();
		*gas_id_info = Default::default();
		let total_num_gases = gas_types_list.len() as usize;
		let mut gas_specific_heat: Vec<f32> = Vec::with_capacity(total_num_gases);
		let mut gas_vis_threshold: Vec<Option<f32>> = Vec::with_capacity(total_num_gases);
		#[cfg(feature = "reaction_hooks")]
		let mut gas_fusion_powers: Vec<f32> = Vec::with_capacity(total_num_gases);
		let meta_gas_visibility_list: auxtools::List = Proc::find("/proc/meta_gas_visibility_list")
			.ok_or_else(|| runtime!("Could not find meta_gas_visibility_list!"))?
			.call(&[])?
			.as_list()?;
		#[cfg(feature = "reaction_hooks")]
		let meta_fusion_powers_list: auxtools::List = Proc::find("/proc/meta_gas_fusion_list")
			.ok_or_else(|| runtime!("Could not find meta_gas_fusion_list!"))?
			.call(&[])?
			.as_list()?;
		for i in 1..gas_types_list.len() + 1 {
			let v = gas_types_list.get(i)?;
			gas_specific_heat.push(gas_types_list.get(&v)?.as_number()?);
			gas_vis_threshold.push(
				meta_gas_visibility_list
					.get(&v)
					.unwrap_or_else(|_| Value::null())
					.as_number()
					.ok(),
			);
			#[cfg(feature = "reaction_hooks")]
			gas_fusion_powers.push(
				meta_fusion_powers_list
					.get(&v)
					.unwrap()
					.as_number()
					.unwrap(),
			);
			gas_id_info
				.id_from_type
				.insert(unsafe { v.raw.data.id }, (i - 1) as usize);
			let gas_str = v.to_string()?;
			if let Some(stripped) = gas_str.strip_prefix("/datum/gas/") {
				gas_id_info
					.id_from_string
					.insert(stripped.to_string(), (i - 1) as usize);
			} else {
				gas_id_info.id_from_string.insert(gas_str, (i - 1) as usize);
			}

			gas_id_info.id_to_type.push(v);
		}
		*GAS_SPECIFIC_HEAT.write() = Some(gas_specific_heat);
		*GAS_VIS_THRESHOLD.write() = Some(gas_vis_threshold);
		#[cfg(feature = "reaction_hooks")]
		FUSION_POWER.with(|f| {
			*f.borrow_mut() = gas_fusion_powers;
		});
		TOTAL_NUM_GASES.store(total_num_gases, Ordering::Release);
		Ok(Value::from(true))
	})
}

fn get_reaction_info() -> Vec<Reaction> {
	let gas_reactions = Value::globals()
		.get(byond_string!("SSair"))
		.unwrap()
		.get_list(byond_string!("gas_reactions"))
		.unwrap();
	let mut reaction_cache: Vec<Reaction> = Vec::with_capacity(gas_reactions.len() as usize);
	for i in 1..gas_reactions.len() + 1 {
		let reaction = &gas_reactions.get(i).unwrap();
		reaction_cache.push(Reaction::from_byond_reaction(&reaction));
	}
	reaction_cache
}

#[hook("/datum/controller/subsystem/air/proc/auxtools_update_reactions")]
fn _update_reactions() {
	*REACTION_INFO.write() = Some(get_reaction_info());
	Ok(Value::from(true))
}

pub fn with_reactions<T, F>(mut f: F) -> T
where
	F: FnMut(&Vec<Reaction>) -> T,
{
	f(&REACTION_INFO
		.read()
		.as_ref()
		.unwrap_or_else(|| panic!("Reactions not loaded yet! Uh oh!")))
}

/// Returns a static reference to a vector of all the specific heats of the gases.
pub fn gas_specific_heat(idx: usize) -> f32 {
	GAS_SPECIFIC_HEAT
		.read()
		.as_ref()
		.unwrap_or_else(|| panic!("Specific heats not loaded yet! Uh oh!"))
		.get(idx as usize)
		.unwrap()
		.clone()
}

#[cfg(feature = "reaction_hooks")]
pub fn gas_fusion_power(idx: &usize) -> f32 {
	FUSION_POWER.with(|g| *g.borrow().get(*idx as usize).unwrap())
}

/// Returns the total number of gases in use. Only used by gas mixtures; should probably stay that way.
pub fn total_num_gases() -> usize {
	TOTAL_NUM_GASES.load(Ordering::Relaxed)
}

/// Gets the gas visibility threshold for the given gas ID.
pub fn gas_visibility(idx: usize) -> Option<f32> {
	GAS_VIS_THRESHOLD
		.read()
		.as_ref()
		.unwrap_or_else(|| panic!("Gas visibility not loaded yet! Uh oh!"))
		.get(idx)
		.unwrap()
		.clone()
}

/// Returns the appropriate index to be used by the game for a given gas datum.
pub fn gas_id_from_type(path: &Value) -> Result<usize, Runtime> {
	GAS_ID_INFO.with(|g| {
		Ok(*g
			.borrow()
			.id_from_type
			.get(&unsafe { path.raw.data.id })
			.ok_or_else(|| runtime!("Invalid type! This should be a gas datum typepath!"))?)
	})
}

/// Takes an index and returns a Value representing the datum typepath of gas datum stored in that index.
pub fn gas_id_to_type(id: usize) -> DMResult {
	GAS_ID_INFO.with(|g| {
		Ok(g.borrow()
			.id_to_type
			.get(id)
			.ok_or_else(|| runtime!("Invalid gas ID: {}", id))?
			.clone())
	})
}

pub fn gas_id_from_type_name(name: &str) -> Result<usize, Runtime> {
	GAS_ID_INFO.with(|g| {
		Ok(*g
			.borrow()
			.id_from_string
			.get(name)
			.ok_or_else(|| runtime!("Invalid gas name: {}", name))?)
	})
}

pub struct GasMixtures {}

use std::convert::From;

struct Index(AtomicUsize);

impl Index {
	fn get(&self) -> Option<usize> {
		let i = self.0.load(Ordering::Relaxed);
		if i.leading_ones() == 0 {
			Some(i)
		} else {
			None
		}
	}
	fn set(&self, i: usize) {
		self.0.store(i, Ordering::Relaxed);
	}
	fn copy(&self, i: &Index) {
		self.0.store(i.0.load(Ordering::Relaxed), Ordering::Relaxed)
	}
	fn invalid() -> Self {
		Self(AtomicUsize::new(usize::MAX))
	}
}

impl From<Index> for Option<usize> {
	fn from(idx: Index) -> Self {
		idx.get()
	}
}

/*
	Much like https://docs.rs/generational-arena/0.2.8/generational_arena/struct.Arena.html,
	but that has properties I don't really need here, so this is my own kinda version.
*/

pub struct Arena<T> {
	internal: Vec<(Index, T)>,
	first_free_idx: Index,
	len: AtomicUsize,
}

impl<T> Arena<T> {
	pub fn new() -> Self {
		Arena {
			internal: Vec::new(),
			first_free_idx: Index::invalid(),
			len: AtomicUsize::new(0),
		}
	}
	pub fn with_capacity(cap: usize) -> Self {
		Arena {
			internal: Vec::with_capacity(cap),
			first_free_idx: Index::invalid(),
			len: AtomicUsize::new(0),
		}
	}
	pub fn get(&self, idx: usize) -> Option<&T> {
		if let Some(e) = self.internal.get(idx) {
			match e.0.get() {
				None => Some(&e.1),
				Some(_) => None,
			}
		} else {
			None
		}
	}
	pub fn len(&self) -> usize {
		self.len.load(Ordering::Relaxed)
	}
	pub fn internal_len(&self) -> usize {
		self.internal.len()
	}
	fn slow_push(&mut self, value: T) -> usize {
		self.internal.push((Index::invalid(), value));
		self.len.store(self.internal.len(), Ordering::Relaxed);
		self.internal.len() - 1
	}
	pub fn push_with(&mut self, f: impl FnOnce() -> T, g: impl FnOnce(&mut T)) -> usize {
		if let Some(idx) = self.first_free_idx.get() {
			let entry = unsafe { self.internal.get_unchecked_mut(idx) };
			self.first_free_idx.copy(&entry.0);
			g(&mut entry.1);
			self.len.fetch_add(1, Ordering::Relaxed);
			idx
		} else {
			self.slow_push(f())
		}
	}
	// technically does not require mut, but absolutely should use mut unless you can guarantee the thing being
	// removed has only one reference to it
	pub unsafe fn remove_unsafe(&self, idx: usize) {
		if let Some(entry) = self.internal.get(idx) {
			entry.0.copy(&self.first_free_idx);
			self.first_free_idx.set(idx);
			self.len.fetch_sub(1, Ordering::Relaxed);
		}
	}
	pub fn remove(&mut self, idx: usize) {
		unsafe {
			self.remove_unsafe(idx);
		}
	}
	pub fn clear(&mut self) {
		for (i, entry) in self.internal.iter_mut().enumerate().rev().skip(1) {
			entry.0.set(i + 1);
		}
		self.len.store(0, Ordering::Relaxed);
		self.first_free_idx.set(0);
	}
	pub fn iter(&self) -> impl Iterator<Item = &T> + '_ {
		self.internal.iter().map(|e| &e.1)
	}
}

impl<V> Arena<RwLock<V>> {
	pub fn try_push(&self, f: impl FnOnce(&mut V)) -> Option<usize> {
		if let Some(idx) = self.first_free_idx.get() {
			let entry = unsafe { self.internal.get_unchecked(idx) };
			self.first_free_idx.copy(&entry.0);
			f(&mut entry.1.write());
			Some(idx)
		} else {
			None
		}
	}
	pub fn proper_push(&mut self, f: impl FnOnce() -> V) -> usize {
		self.slow_push(RwLock::new(f()))
	}
}

/*
	This is where the gases live.
	This is just a big vector, acting as a gas mixture pool.
	As you can see, it can be accessed by any thread at any time;
	of course, it has a RwLock preventing this, and you can't access the
	vector directly. Seriously, please don't. I have the wrapper functions for a reason.
*/
lazy_static! {
	static ref GAS_MIXTURES: RwLock<Arena<RwLock<GasMixture>>> =
		RwLock::new(Arena::with_capacity(100000));
}

impl GasMixtures {
	pub fn with_all_mixtures<T, F>(mut f: F) -> T
	where
		F: FnMut(&Arena<RwLock<GasMixture>>) -> T,
	{
		f(&GAS_MIXTURES.read())
	}
	fn with_gas_mixture<T, F>(id: f32, mut f: F) -> Result<T, Runtime>
	where
		F: FnMut(&GasMixture) -> Result<T, Runtime>,
	{
		let mixtures = GAS_MIXTURES.read();
		let mix = mixtures
			.get(id.to_bits() as usize)
			.ok_or_else(|| runtime!("No gas mixture with ID {} exists!", id.to_bits()))?
			.read();
		f(&mix)
	}
	fn with_gas_mixture_mut<T, F>(id: f32, mut f: F) -> Result<T, Runtime>
	where
		F: FnMut(&mut GasMixture) -> Result<T, Runtime>,
	{
		let gas_mixtures = GAS_MIXTURES.read();
		let mut mix = gas_mixtures
			.get(id.to_bits() as usize)
			.ok_or_else(|| runtime!("No gas mixture with ID {} exists!", id.to_bits()))?
			.write();
		f(&mut mix)
	}
	fn with_gas_mixtures<T, F>(src: f32, arg: f32, mut f: F) -> Result<T, Runtime>
	where
		F: FnMut(&GasMixture, &GasMixture) -> Result<T, Runtime>,
	{
		let gas_mixtures = GAS_MIXTURES.read();
		let src_gas = gas_mixtures
			.get(src.to_bits() as usize)
			.ok_or_else(|| runtime!("No gas mixture with ID {} exists!", src.to_bits()))?
			.read();
		let arg_gas = gas_mixtures
			.get(arg.to_bits() as usize)
			.ok_or_else(|| runtime!("No gas mixture with ID {} exists!", arg.to_bits()))?
			.read();
		f(&src_gas, &arg_gas)
	}
	fn with_gas_mixtures_mut<T, F>(src: f32, arg: f32, mut f: F) -> Result<T, Runtime>
	where
		F: FnMut(&mut GasMixture, &mut GasMixture) -> Result<T, Runtime>,
	{
		let src = src.to_bits() as usize;
		let arg = arg.to_bits() as usize;
		let gas_mixtures = GAS_MIXTURES.read();
		if src == arg {
			let mut entry = gas_mixtures
				.get(src)
				.ok_or_else(|| runtime!("No gas mixture with ID {} exists!", src))?
				.write();
			let mix = &mut entry;
			let mut copied = mix.clone();
			f(mix, &mut copied)
		} else {
			f(
				&mut gas_mixtures
					.get(src)
					.ok_or_else(|| runtime!("No gas mixture with ID {} exists!", src))?
					.write(),
				&mut gas_mixtures
					.get(arg)
					.ok_or_else(|| runtime!("No gas mixture with ID {} exists!", arg))?
					.write(),
			)
		}
	}
	fn with_gas_mixtures_custom<T, F>(src: f32, arg: f32, mut f: F) -> Result<T, Runtime>
	where
		F: FnMut(&RwLock<GasMixture>, &RwLock<GasMixture>) -> Result<T, Runtime>,
	{
		let src = src.to_bits() as usize;
		let arg = arg.to_bits() as usize;
		let gas_mixtures = GAS_MIXTURES.read();
		if src == arg {
			let entry = gas_mixtures
				.get(src)
				.ok_or_else(|| runtime!("No gas mixture with ID {} exists!", src))?;
			f(entry, entry.clone())
		} else {
			f(
				gas_mixtures
					.get(src)
					.ok_or_else(|| runtime!("No gas mixture with ID {} exists!", src))?,
				gas_mixtures
					.get(arg)
					.ok_or_else(|| runtime!("No gas mixture with ID {} exists!", arg))?,
			)
		}
	}
	/// Fills in the first unused slot in the gas mixtures vector, or adds another one, then sets the argument Value to point to it.
	pub fn register_gasmix(mix: &Value) -> DMResult {
		let vol = mix.get_number(byond_string!("initial_volume"))?;
		if let Some(idx) = {
			let lock = GAS_MIXTURES.read();
			lock.try_push(|g| g.clear_with_vol(vol))
		} {
			mix.set(
				byond_string!("_extools_pointer_gasmixture"),
				f32::from_bits(idx as u32),
			)?;
		} else {
			loop {
				if let Some(mut lock) =
					GAS_MIXTURES.try_write_for(std::time::Duration::from_micros(500))
				{
					mix.set(
						byond_string!("_extools_pointer_gasmixture"),
						f32::from_bits(lock.proper_push(|| GasMixture::from_vol(vol)) as u32),
					)?;
					break;
				}
			}
		}
		Ok(Value::null())
	}
	/// Marks the Value's gas mixture as unused, allowing it to be reallocated to another.
	pub fn unregister_gasmix(mix: &Value) -> DMResult {
		if let Ok(float_bits) = mix.get_number(byond_string!("_extools_pointer_gasmixture")) {
			let idx = float_bits.to_bits();
			loop {
				if let Some(mut lock) = GAS_MIXTURES.try_write_for(std::time::Duration::from_micros(500)) {
					lock.remove(idx as usize);
					break;
				};
			}
			mix.set(byond_string!("_extools_pointer_gasmixture"), &Value::null())?;
		}
		Ok(Value::null())
	}
}

#[shutdown]
fn _shut_down_gases() {
	GAS_MIXTURES.write().clear();
}

/// Gets the mix for the given value, and calls the provided closure with a reference to that mix as an argument.
pub fn with_mix<T, F>(mix: &Value, f: F) -> Result<T, Runtime>
where
	F: FnMut(&GasMixture) -> Result<T, Runtime>,
{
	GasMixtures::with_gas_mixture(
		mix.get_number(byond_string!("_extools_pointer_gasmixture"))?,
		f,
	)
}

/// As with_mix, but mutable.
pub fn with_mix_mut<T, F>(mix: &Value, f: F) -> Result<T, Runtime>
where
	F: FnMut(&mut GasMixture) -> Result<T, Runtime>,
{
	GasMixtures::with_gas_mixture_mut(
		mix.get_number(byond_string!("_extools_pointer_gasmixture"))?,
		f,
	)
}

/// As with_mix, but with two mixes.
pub fn with_mixes<T, F>(src_mix: &Value, arg_mix: &Value, f: F) -> Result<T, Runtime>
where
	F: FnMut(&GasMixture, &GasMixture) -> Result<T, Runtime>,
{
	GasMixtures::with_gas_mixtures(
		src_mix.get_number(byond_string!("_extools_pointer_gasmixture"))?,
		arg_mix.get_number(byond_string!("_extools_pointer_gasmixture"))?,
		f,
	)
}

/// As with_mix_mut, but with two mixes.
pub fn with_mixes_mut<T, F>(src_mix: &Value, arg_mix: &Value, f: F) -> Result<T, Runtime>
where
	F: FnMut(&mut GasMixture, &mut GasMixture) -> Result<T, Runtime>,
{
	GasMixtures::with_gas_mixtures_mut(
		src_mix.get_number(byond_string!("_extools_pointer_gasmixture"))?,
		arg_mix.get_number(byond_string!("_extools_pointer_gasmixture"))?,
		f,
	)
}

/// Allows different lock levels for each gas. Instead of relevant refs to the gases, returns the RWLock object.
pub fn with_mixes_custom<T, F>(src_mix: &Value, arg_mix: &Value, f: F) -> Result<T, Runtime>
where
	F: FnMut(&RwLock<GasMixture>, &RwLock<GasMixture>) -> Result<T, Runtime>,
{
	GasMixtures::with_gas_mixtures_custom(
		src_mix.get_number(byond_string!("_extools_pointer_gasmixture"))?,
		arg_mix.get_number(byond_string!("_extools_pointer_gasmixture"))?,
		f,
	)
}

#[hook("/proc/fix_corrupted_atmos")]
fn _fix_corrupted_atmos() {
	rayon::spawn(|| {
		for lock in GAS_MIXTURES.read().iter().filter(|lock| {
			if let Some(gas) = lock.try_read() {
				gas.is_corrupt()
			} else {
				false
			}
		}) {
			lock.write().fix_corruption();
		}
	});
	Ok(Value::null())
}

pub(crate) fn amt_gases() -> usize {
	GAS_MIXTURES.read().len()
}

pub(crate) fn tot_gases() -> usize {
	GAS_MIXTURES.read().internal_len()
}
